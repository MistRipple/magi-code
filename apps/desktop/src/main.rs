#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    env, fs,
    path::PathBuf,
    process,
    sync::{Arc, Mutex},
    time::Duration,
};

use magi_daemon::{Daemon, DaemonConfig, DaemonHandle};
use magi_desktop::lifecycle::{DesktopAction, DesktopLifecycle, DesktopState};
use magi_runtime_state::RuntimeStateManager;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    ipc::Channel,
    menu::{Menu, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_updater::UpdaterExt;

const MAIN_WINDOW_LABEL: &str = "main";
const OPEN_MENU_ID: &str = "open-magi";
const QUIT_MENU_ID: &str = "quit-magi";
const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 38123;
const DEFAULT_SERVICE_NAME: &str = "magi-rust-backend";
const DESKTOP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DESKTOP_UPDATE_DIRECTORY: &str = "updates";
const STAGED_UPDATE_BYTES_FILE: &str = "pending-update.bin";
const STAGED_UPDATE_METADATA_FILE: &str = "pending-update.json";

struct DesktopRuntime {
    lifecycle: Arc<DesktopLifecycle>,
    daemon: Arc<Mutex<Option<DaemonHandle>>>,
    state_root: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StagedDesktopUpdate {
    current_version: String,
    version: String,
    date: Option<String>,
    body: Option<String>,
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
    fn new(state_root: PathBuf) -> Self {
        Self {
            lifecycle: Arc::new(DesktopLifecycle::new()),
            daemon: Arc::new(Mutex::new(None)),
            state_root,
        }
    }
}

fn default_state_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".magi")
}

fn staged_update_paths(state_root: &PathBuf) -> (PathBuf, PathBuf) {
    let directory = state_root.join(DESKTOP_UPDATE_DIRECTORY);
    (
        directory.join(STAGED_UPDATE_BYTES_FILE),
        directory.join(STAGED_UPDATE_METADATA_FILE),
    )
}

fn remove_staged_update(state_root: &PathBuf) {
    let (bytes_path, metadata_path) = staged_update_paths(state_root);
    let _ = fs::remove_file(bytes_path);
    let _ = fs::remove_file(metadata_path);
}

fn read_staged_update(state_root: &PathBuf) -> Result<Option<StagedDesktopUpdate>, String> {
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

fn write_staged_update(
    state_root: &PathBuf,
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

fn parse_desktop_web_url(value: &str) -> Result<tauri::Url, String> {
    let mut url = value
        .parse::<tauri::Url>()
        .map_err(|error| format!("Magi Web 地址非法: {error}"))?;
    url.query_pairs_mut()
        .append_pair("desktopVersion", env!("CARGO_PKG_VERSION"));
    Ok(url)
}

fn create_startup_error_window(app: &AppHandle) {
    if let Err(error) = create_main_window(app, WebviewUrl::App("index.html".into())) {
        eprintln!("创建 Magi 启动错误窗口失败: {error}");
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
    // 更新安装前不能等待活动请求结束；释放句柄会立即中止 daemon 服务任务和受管进程。
    drop(daemon.lock().expect("desktop daemon lock poisoned").take());

    let runtime_state = RuntimeStateManager::new(state_root.join("runtime"));
    runtime_state.remove_runtime_state();
    runtime_state.remove_pid();
    lifecycle.mark_stopped();
}

#[tauri::command]
fn prepare_update_restart(app: AppHandle) -> Result<(), String> {
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
async fn get_staged_desktop_update(app: AppHandle) -> Result<Option<StagedDesktopUpdate>, String> {
    let state_root = app.state::<DesktopRuntime>().state_root.clone();
    read_staged_update(&state_root)
}

#[tauri::command]
async fn stage_desktop_update(
    app: AppHandle,
    version: String,
    on_event: Channel<DesktopUpdateDownloadEvent>,
) -> Result<StagedDesktopUpdate, String> {
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
        return Err(format!(
            "已下载版本 v{} 与当前可用版本 v{} 不一致，请重新下载",
            staged.version, update.version
        ));
    }

    update
        .install(bytes)
        .map_err(|error| format!("安装更新失败: {error}"))?;
    remove_staged_update(&state_root);
    Ok(())
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
            eprintln!("Magi 桌面包缺少内置 Web 入口: {}", web_dist_root.display());
            let runtime = app.state::<DesktopRuntime>();
            runtime.lifecycle.mark_ready();
            create_startup_error_window(&app);
            return;
        }

        let port = match read_port() {
            Ok(port) => port,
            Err(error) => {
                eprintln!("{error}");
                let runtime = app.state::<DesktopRuntime>();
                runtime.lifecycle.mark_ready();
                create_startup_error_window(&app);
                return;
            }
        };
        let host = read_env("MAGI_HOST").unwrap_or_else(|| DEFAULT_HOST.to_string());
        let service_name =
            read_env("MAGI_SERVICE_NAME").unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
        let config = DaemonConfig::new(host.clone(), port, service_name, &state_root)
            .with_web_dist_root(web_dist_root)
            .with_open_browser(false);

        let handle = match Daemon::new(config).start().await {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("Magi daemon 启动失败: {error}");
                let runtime = app.state::<DesktopRuntime>();
                runtime.lifecycle.mark_ready();
                create_startup_error_window(&app);
                return;
            }
        };
        let web_url = handle.web_url().to_string();
        let bound_addr = handle.bound_addr();
        let runtime_state = RuntimeStateManager::new(state_root.join("runtime"));
        runtime_state.write_runtime_state(process::id(), Some(&host), bound_addr.port());
        runtime_state.write_pid(process::id());

        let runtime = app.state::<DesktopRuntime>();
        if matches!(
            runtime.lifecycle.state(),
            DesktopState::ShuttingDown | DesktopState::Restarting | DesktopState::Stopped
        ) {
            drop(handle);
            runtime_state.remove_runtime_state();
            runtime_state.remove_pid();
            runtime.lifecycle.mark_stopped();
            app.exit(0);
            return;
        }
        *runtime.daemon.lock().expect("desktop daemon lock poisoned") = Some(handle);
        runtime.lifecycle.mark_ready();

        match parse_desktop_web_url(&web_url) {
            Ok(url) => {
                if let Err(error) = create_main_window(&app, WebviewUrl::External(url)) {
                    eprintln!("创建 Magi 主窗口失败: {error}");
                    request_exit(app);
                }
            }
            Err(error) => {
                eprintln!("{error}");
                request_exit(app);
            }
        }
    });
}

fn main() {
    magi_process::initialize_user_process_environment();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            prepare_update_restart,
            get_staged_desktop_update,
            stage_desktop_update,
            install_staged_desktop_update,
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
            app.manage(DesktopRuntime::new(state_root.clone()));
            install_tray(app)?;

            let web_dist_root = resolve_web_dist_root(app)?;
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
        let url = parse_desktop_web_url("http://127.0.0.1:38123/web.html")
            .expect("desktop web URL should parse");

        assert_eq!(
            url.as_str(),
            concat!(
                "http://127.0.0.1:38123/web.html?desktopVersion=",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}

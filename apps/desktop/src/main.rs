#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    env,
    path::PathBuf,
    process,
    sync::{Arc, Mutex},
};

use magi_daemon::{Daemon, DaemonConfig, DaemonHandle};
use magi_desktop::lifecycle::{DesktopAction, DesktopLifecycle, DesktopState};
use magi_runtime_state::RuntimeStateManager;
use tauri::{
    AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    menu::{Menu, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconBuilder,
};

const MAIN_WINDOW_LABEL: &str = "main";
const OPEN_MENU_ID: &str = "open-magi";
const QUIT_MENU_ID: &str = "quit-magi";
const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 38123;
const DEFAULT_SERVICE_NAME: &str = "magi-rust-backend";

struct DesktopRuntime {
    lifecycle: Arc<DesktopLifecycle>,
    daemon: Arc<Mutex<Option<DaemonHandle>>>,
    state_root: PathBuf,
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
        if let Err(error) = handle.wait().await {
            eprintln!("等待 Magi daemon 关闭失败: {error}");
        }
    }

    let runtime_state = RuntimeStateManager::new(state_root.join("runtime"));
    runtime_state.remove_runtime_state();
    runtime_state.remove_pid();
    lifecycle.mark_stopped();
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
            DesktopState::ShuttingDown | DesktopState::Stopped
        ) {
            let _ = handle.shutdown("desktop exited during startup");
            let _ = handle.wait().await;
            runtime_state.remove_runtime_state();
            runtime_state.remove_pid();
            runtime.lifecycle.mark_stopped();
            app.exit(0);
            return;
        }
        *runtime.daemon.lock().expect("desktop daemon lock poisoned") = Some(handle);
        runtime.lifecycle.mark_ready();

        match web_url.parse() {
            Ok(url) => {
                if let Err(error) = create_main_window(&app, WebviewUrl::External(url)) {
                    eprintln!("创建 Magi 主窗口失败: {error}");
                    request_exit(app);
                }
            }
            Err(error) => {
                eprintln!("Magi Web 地址非法: {error}");
                request_exit(app);
            }
        }
    });
}

fn main() {
    let app = tauri::Builder::default()
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
            if runtime.lifecycle.state() != DesktopState::Stopped {
                api.prevent_exit();
                request_exit(app.clone());
            }
        }
        RunEvent::Exit => {
            let runtime = app.state::<DesktopRuntime>();
            if runtime.lifecycle.state() == DesktopState::Stopped {
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

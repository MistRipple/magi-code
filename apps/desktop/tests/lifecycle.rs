use magi_desktop::lifecycle::{DesktopAction, DesktopLifecycle, DesktopState};

const DESKTOP_MAIN_SOURCE: &str = include_str!("../src/main.rs");

#[test]
fn native_window_header_does_not_repeat_product_name() {
    assert!(DESKTOP_MAIN_SOURCE.contains(".title(\"\")"));
    assert!(!DESKTOP_MAIN_SOURCE.contains(".title(\"Magi\")"));
}

#[test]
fn explicit_exit_closes_webview_before_waiting_for_daemon() {
    let request_exit = DESKTOP_MAIN_SOURCE
        .split("fn request_exit(app: AppHandle)")
        .nth(1)
        .and_then(|source| source.split("async fn shutdown_desktop_runtime").next())
        .expect("request_exit source should exist");
    let close_index = request_exit
        .find("window.close()")
        .expect("request_exit should close the webview");
    let spawn_index = request_exit
        .find("tauri::async_runtime::spawn")
        .expect("request_exit should spawn daemon shutdown");
    assert!(close_index < spawn_index);
}

#[test]
fn close_hides_ready_window_without_stopping_service() {
    let lifecycle = DesktopLifecycle::new();
    assert_eq!(lifecycle.mark_ready(), DesktopAction::ShowWindow);
    assert_eq!(lifecycle.request_window_close(), DesktopAction::HideWindow);
    assert_eq!(lifecycle.state(), DesktopState::ReadyHidden);
}

#[test]
fn tray_open_restores_hidden_window() {
    let lifecycle = DesktopLifecycle::new();
    lifecycle.mark_ready();
    lifecycle.request_window_close();

    assert_eq!(lifecycle.request_show(), DesktopAction::ShowWindow);
    assert_eq!(lifecycle.state(), DesktopState::ReadyVisible);
}

#[test]
fn explicit_exit_is_single_and_terminal() {
    let lifecycle = DesktopLifecycle::new();
    lifecycle.mark_ready();

    assert_eq!(lifecycle.request_exit(), DesktopAction::BeginExit);
    assert_eq!(lifecycle.request_exit(), DesktopAction::Ignore);
    assert_eq!(lifecycle.request_window_close(), DesktopAction::Ignore);
    assert_eq!(lifecycle.state(), DesktopState::ShuttingDown);

    assert_eq!(lifecycle.mark_stopped(), DesktopAction::ExitProcess);
    assert_eq!(lifecycle.state(), DesktopState::Stopped);
}

#[test]
fn update_restart_has_a_separate_lifecycle_transition() {
    let lifecycle = DesktopLifecycle::new();
    lifecycle.mark_ready();

    assert_eq!(lifecycle.request_update_restart(), DesktopAction::BeginExit);
    assert_eq!(lifecycle.state(), DesktopState::Restarting);
    assert_eq!(lifecycle.request_update_restart(), DesktopAction::Ignore);
    assert_eq!(lifecycle.request_window_close(), DesktopAction::Ignore);
    assert_eq!(lifecycle.mark_stopped(), DesktopAction::ExitProcess);
    assert_eq!(lifecycle.state(), DesktopState::Stopped);
}

#[test]
fn update_restart_forces_daemon_stop_before_install() {
    assert!(DESKTOP_MAIN_SOURCE.contains("DESKTOP_SHUTDOWN_TIMEOUT"));
    assert!(DESKTOP_MAIN_SOURCE.contains("fn force_shutdown_desktop_runtime"));
    assert!(DESKTOP_MAIN_SOURCE.contains("request_update_restart()"));
    assert!(DESKTOP_MAIN_SOURCE.contains("DesktopState::ShuttingDown | DesktopState::Restarting"));

    let update_command = DESKTOP_MAIN_SOURCE
        .split("fn prepare_update_restart(app: AppHandle)")
        .nth(1)
        .and_then(|source| source.split("fn install_tray").next())
        .expect("update restart command should exist");
    assert!(update_command.contains("force_shutdown_desktop_runtime"));
    assert!(!update_command.contains("\n            shutdown_desktop_runtime("));
}

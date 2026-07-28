use std::collections::BTreeSet;

use serde_json::Value;

const BUILD_SOURCE: &str = include_str!("../build.rs");
const CAPABILITY_SOURCE: &str = include_str!("../capabilities/default.json");
const DESKTOP_MAIN_SOURCE: &str = include_str!("../src/main.rs");

const UPDATE_COMMANDS: [&str; 5] = [
    "prepare_update_restart",
    "get_staged_desktop_update",
    "get_desktop_update_installability",
    "stage_desktop_update",
    "install_staged_desktop_update",
];
const RUNTIME_RECOVERY_COMMANDS: [&str; 2] =
    ["get_desktop_runtime_recovery", "restart_desktop_runtime"];

#[test]
fn remote_desktop_origin_has_only_the_required_update_command_permissions() {
    let capability: Value =
        serde_json::from_str(CAPABILITY_SOURCE).expect("desktop capability must be valid JSON");
    let permissions = capability["permissions"]
        .as_array()
        .expect("desktop capability must define permissions")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();

    for command in UPDATE_COMMANDS {
        let permission = format!("allow-{}", command.replace('_', "-"));
        assert!(
            permissions.contains(permission.as_str()),
            "desktop remote origin must be granted {permission}"
        );
        assert!(
            BUILD_SOURCE.contains(&format!("\"{command}\"")),
            "Tauri app manifest must generate the ACL permission for {command}"
        );
        assert!(
            DESKTOP_MAIN_SOURCE.contains(&format!("            {command},")),
            "Tauri invoke handler must register {command}"
        );
    }

    let update_permissions = permissions
        .iter()
        .filter(|permission| permission.starts_with("allow-") && permission.contains("update"))
        .copied()
        .collect::<BTreeSet<_>>();
    let expected_permissions = UPDATE_COMMANDS
        .iter()
        .map(|command| format!("allow-{}", command.replace('_', "-")))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        update_permissions,
        expected_permissions.iter().map(String::as_str).collect(),
        "desktop update commands must keep an explicit least-privilege ACL"
    );
}

#[test]
fn remote_desktop_origin_has_only_explicit_runtime_recovery_commands() {
    let capability: Value =
        serde_json::from_str(CAPABILITY_SOURCE).expect("desktop capability must be valid JSON");
    let permissions = capability["permissions"]
        .as_array()
        .expect("desktop capability must define permissions")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();

    for command in RUNTIME_RECOVERY_COMMANDS {
        let permission = format!("allow-{}", command.replace('_', "-"));
        assert!(permissions.contains(permission.as_str()));
        assert!(BUILD_SOURCE.contains(&format!("\"{command}\"")));
        assert!(DESKTOP_MAIN_SOURCE.contains(&format!("            {command},")));
    }
    assert!(
        DESKTOP_MAIN_SOURCE.contains("confirm_external_processes"),
        "ending a non-Magi listener must require an explicit confirmation field"
    );
    assert!(
        DESKTOP_MAIN_SOURCE.contains("terminate_port_occupants"),
        "runtime restart must use the shared identity-validating port cleanup"
    );
}

#[test]
fn remote_desktop_origin_can_only_open_web_urls_externally() {
    let capability: Value =
        serde_json::from_str(CAPABILITY_SOURCE).expect("desktop capability must be valid JSON");
    let opener_permission = capability["permissions"]
        .as_array()
        .expect("desktop capability must define permissions")
        .iter()
        .find(|permission| permission["identifier"] == "opener:allow-open-url")
        .expect("desktop capability must allow opening external web URLs");
    let allowed_urls = opener_permission["allow"]
        .as_array()
        .expect("opener permission must define URL scopes")
        .iter()
        .filter_map(|scope| scope["url"].as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(allowed_urls, BTreeSet::from(["http://*", "https://*"]));
    assert!(
        DESKTOP_MAIN_SOURCE.contains(".plugin(tauri_plugin_opener::init())"),
        "desktop host must register the official Tauri opener plugin"
    );
    assert!(
        !CAPABILITY_SOURCE.contains("opener:default")
            && !CAPABILITY_SOURCE.contains("opener:allow-open-path")
            && !CAPABILITY_SOURCE.contains("opener:allow-reveal-item-in-dir"),
        "desktop external-link capability must not grant filesystem opener permissions"
    );
}

use std::collections::BTreeSet;

use serde_json::Value;

const BUILD_SOURCE: &str = include_str!("../build.rs");
const CAPABILITY_SOURCE: &str = include_str!("../capabilities/default.json");
const DESKTOP_MAIN_SOURCE: &str = include_str!("../src/main.rs");

const UPDATE_COMMANDS: [&str; 4] = [
    "prepare_update_restart",
    "get_staged_desktop_update",
    "stage_desktop_update",
    "install_staged_desktop_update",
];

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

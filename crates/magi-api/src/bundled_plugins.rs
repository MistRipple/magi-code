//! 第一方插件包目录。与外部插件共用包校验、安装和生命周期。

use crate::errors::ApiError;
use magi_plugin_system::PluginPackage;

/// Magi MCP 服务（对外开放工作区工具）的第一方插件身份。
pub const MCP_SERVER_PLUGIN_ID: &str = "magi.mcp-server";

/// 随 Magi 发布的第一方插件清单。新增第一方插件须同时登记到这里和 `BUILTIN_PLUGIN_IDS`。
pub(crate) fn bundled_plugin_manifests() -> Vec<magi_app_server_protocol::PluginManifest> {
    vec![gpt_web_plugin_manifest(), mcp_server_plugin_manifest()]
}

pub(crate) fn bundled_plugin_package(id: &str) -> Result<PluginPackage, ApiError> {
    let manifest = bundled_plugin_manifests()
        .into_iter()
        .find(|manifest| manifest.id == id)
        .ok_or_else(|| ApiError::NotFound("内置插件不存在".into()))?;
    PluginPackage::from_manifest_and_source(manifest, "export default (input) => input")
        .map_err(|error| ApiError::internal_assembly("生成内置插件包失败", error))
}

/// MCP 服务插件只是安装与激活的壳：监听、令牌、隧道与审计都是 Magi 内部能力。
/// 激活后才监听端口并按已保存的配置恢复；停用即释放，已有令牌与配置保留。
fn mcp_server_plugin_manifest() -> magi_app_server_protocol::PluginManifest {
    serde_json::from_value(serde_json::json!({
        "sdkVersion": 1,
        "id": MCP_SERVER_PLUGIN_ID,
        "version": "1.0.0",
        "name": "Magi MCP 服务",
        "description": "Expose workspace tools to external MCP clients (Cursor, Claude Desktop, …) with tokens, approvals and audit.",
        "backend": "plugin.mjs",
        "permissions": [
            {"kind": "tools", "scope": "application", "targets": ["workspace-tools"]},
            {"kind": "network", "scope": "application", "targets": ["local-listener", "tunnel"]}
        ],
        "contributions": {},
        "settingsSchema": {"type": "object", "additionalProperties": false},
        "dataSchemaVersion": 1
    }))
    .expect("MCP server bundled manifest must satisfy the generated contract")
}

pub(crate) fn gpt_web_plugin_manifest() -> magi_app_server_protocol::PluginManifest {
    serde_json::from_value(serde_json::json!({
        "sdkVersion": 1,
        "id": magi_web_model::WEB_MODEL_PLUGIN_ID,
        "version": "1.1.0",
        "name": "GPT Web",
        "description": "Use ChatGPT Web as an optional Magi session engine.",
        "backend": "plugin.mjs",
        "permissions": [
            {"kind": "browser", "scope": "application", "targets": ["gpt-web"]},
            {"kind": "models", "scope": "application", "targets": ["default"]}
        ],
        "contributions": {
            "engines": [{"id": "default", "title": "GPT Web", "description": "ChatGPT Web session engine"}],
            "browserSurfaces": [{
                "id": "gpt-web",
                "title": "GPT Web",
                "description": "ChatGPT login and conversation page owned by BrowserAuthority",
                "scope": "application",
                "persistence": "persistent",
                "keepMounted": true,
                "closeAction": "hide",
                "backgroundInference": true
            }]
        },
        "settingsSchema": {"type": "object", "additionalProperties": false},
        "dataSchemaVersion": 1
    }))
    .expect("GPT Web bundled manifest must satisfy the generated contract")
}

/// 测试辅助：安装并在应用作用域激活一个第一方插件（授权其清单声明的全部应用级权限）。
#[cfg(test)]
pub(crate) fn activate_bundled_for_test(state: &crate::state::ApiState, id: &str) {
    let package = bundled_plugin_package(id).unwrap();
    let grants = package.manifest().permissions.clone();
    let mut manager = state.plugin_manager.lock().unwrap();
    manager
        .install(
            &package,
            magi_plugin_system::PluginSource::Builtin { name: id.into() },
        )
        .unwrap();
    manager.enable(id, "application").unwrap();
    manager.authorize(id, "application", grants).unwrap();
    manager.activate(id, "application").unwrap();
}

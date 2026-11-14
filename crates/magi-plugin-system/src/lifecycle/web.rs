//! 插件联网提供方选择与安装共用 daemon 状态；工具执行仍沿原有授权入口。
use super::*;
use magi_app_server_protocol::generated::{
    PluginWebBinding, PluginWebConfiguration, PluginWebConfigureRequest, PluginWebKind,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WebConfiguration {
    revision: i64,
    search: Option<PluginWebBinding>,
    fetch: Option<PluginWebBinding>,
}

impl PluginManager {
    pub fn web_configuration(&self) -> PluginWebConfiguration {
        PluginWebConfiguration {
            revision: self.state.web.revision,
            search: self.state.web.search.clone(),
            fetch: self.state.web.fetch.clone(),
        }
    }

    pub fn configure_web(
        &mut self,
        plugin: &str,
        lease: &str,
        input: PluginWebConfigureRequest,
    ) -> Result<PluginWebConfiguration, PluginError> {
        self.check_runtime(plugin, lease)?;
        if input.expected_revision != self.state.web.revision {
            return Err(conflict("联网提供方选择已变化，请重新读取"));
        }
        if input.changes.is_empty() || input.changes.len() > 2 {
            return Err(invalid("联网提供方修改须包含一至两项"));
        }
        let manifest = self.runtime_manifest(plugin, true)?;
        let mut next = self.state.clone();
        let mut changed = BTreeSet::new();
        for change in input.changes {
            let kind = match change.kind {
                PluginWebKind::Search => "search",
                PluginWebKind::Fetch => "fetch",
            };
            if !changed.insert(kind)
                || !self.permission_allowed(plugin, PluginPermissionKind::WebProviders, kind)?
            {
                return Err(PluginError::NotAuthorized(
                    "联网提供方修改重复或未授权".into(),
                ));
            }
            let binding = match (change.contribution_id, change.provider_id) {
                (None, None) => None,
                (Some(id), Some(provider)) => {
                    if provider.is_empty()
                        || provider.len() > 96
                        || !manifest
                            .contributions
                            .tools
                            .iter()
                            .any(|tool| tool.id == id && tool.read_only)
                        || !self.permission_allowed(plugin, PluginPermissionKind::Tools, &id)?
                    {
                        return Err(invalid("联网提供方必须绑定本插件已授权的只读工具"));
                    }
                    Some(PluginWebBinding {
                        plugin_id: plugin.into(),
                        contribution_id: id,
                        provider_id: provider,
                    })
                }
                _ => return Err(invalid("联网提供方身份不完整")),
            };
            match change.kind {
                PluginWebKind::Search => next.web.search = binding,
                PluginWebKind::Fetch => next.web.fetch = binding,
            }
        }
        if next.web != self.state.web {
            next.web.revision = next
                .web
                .revision
                .checked_add(1)
                .ok_or_else(|| conflict("联网配置版本耗尽"))?;
            self.commit(next)?;
        }
        Ok(self.web_configuration())
    }

    /// 停用／卸载回到原生实现；已启用但未就绪仍保留选择，由真实调用明确失败。
    pub fn selected_web_tool(&self, kind: &PluginWebKind) -> Result<Option<String>, PluginError> {
        let binding = match kind {
            PluginWebKind::Search => &self.state.web.search,
            PluginWebKind::Fetch => &self.state.web.fetch,
        };
        let Some(binding) = binding else {
            return Ok(None);
        };
        let Some(installed) = self.state.plugins.get(&binding.plugin_id) else {
            return Ok(None);
        };
        if !installed.enabled {
            return Ok(None);
        }
        let target = match kind {
            PluginWebKind::Search => "search",
            PluginWebKind::Fetch => "fetch",
        };
        if !self.permission_allowed(
            &binding.plugin_id,
            PluginPermissionKind::WebProviders,
            target,
        )? {
            return Err(PluginError::NotAuthorized("联网提供方授权已撤销".into()));
        }
        if self.runtime_ready(&binding.plugin_id)?
            && !self
                .effective_manifest(&binding.plugin_id)?
                .contributions
                .tools
                .iter()
                .any(|tool| tool.id == binding.contribution_id && tool.read_only)
        {
            return Err(conflict("当前联网提供方已撤销或不再是只读工具"));
        }
        Ok(Some(crate::model_tool_name(
            &binding.plugin_id,
            &binding.contribution_id,
        )))
    }
}

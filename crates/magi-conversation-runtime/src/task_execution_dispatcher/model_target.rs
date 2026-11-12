use super::*;

impl LlmTaskDispatcher {
    pub(super) fn execution_settings_snapshot(&self) -> Option<Arc<SettingsStore>> {
        self.settings_store
            .as_ref()
            .map(|store| Arc::new(store.execution_snapshot()))
    }

    pub(super) fn execution_settings_or_live<'a>(
        &'a self,
        execution_settings_snapshot: Option<&'a Arc<SettingsStore>>,
    ) -> Option<&'a Arc<SettingsStore>> {
        execution_settings_snapshot.or(self.settings_store.as_ref())
    }

    pub(super) fn resolve_target_cached(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        target: RoleTarget<'_>,
        session_id: Option<&SessionId>,
        thread_id: &str,
    ) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
        let Some(settings_store) = settings_store else {
            return resolve_target_for_role(
                settings_store,
                self.model_bridge_client.clone(),
                target,
                session_id,
            );
        };
        let target_label = match target {
            RoleTarget::Orchestrator => "orchestrator".to_string(),
            RoleTarget::Auxiliary => "auxiliary".to_string(),
            RoleTarget::Agent { role_id } => format!("agent:{role_id}"),
        };
        let cache_key = format!(
            "{}\u{001f}{}\u{001f}{}\u{001f}{}",
            target_label,
            session_id.map(SessionId::as_str).unwrap_or_default(),
            thread_id,
            settings_store.revision(),
        );
        if let Some(client) = self
            .model_client_cache
            .lock()
            .expect("model client cache lock poisoned")
            .get(&cache_key)
            .cloned()
        {
            return Ok(Some(client));
        }
        let resolved = resolve_target_for_role(
            Some(settings_store),
            self.model_bridge_client.clone(),
            target,
            session_id,
        )?;
        if let Some(client) = resolved.as_ref() {
            self.model_client_cache
                .lock()
                .expect("model client cache lock poisoned")
                .insert(cache_key, client.clone());
        }
        Ok(resolved)
    }

    /// 扩展调用的模型必须与 `resolve_model_client_for_task` 选中的角色一致，并拒绝插件主对话数据源。
    pub(crate) fn ensure_extension_model_source(
        &self,
        task: Option<&magi_core::Task>,
        execution_role_id: Option<&str>,
        session_id: &SessionId,
    ) -> Result<(), String> {
        let Some(store) = self.settings_store.as_ref() else {
            return Ok(());
        };
        if task
            .and_then(|task| task.executor_binding.as_ref())
            .and_then(|binding| binding.model_selection.as_ref())
            .is_some()
        {
            return Ok(());
        }
        let role_id = execution_role_id
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .or_else(|| task.and_then(|task| task_role_id(Some(task))));
        crate::model_config::ensure_extension_model_source(store, role_id, session_id)
    }

    pub(super) fn resolve_model_client_for_task(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        task: Option<&magi_core::Task>,
        execution_role_id: Option<&str>,
        session_id: Option<&SessionId>,
    ) -> Result<Arc<dyn ModelBridgeClient>, String> {
        if let Some(selection) = task
            .and_then(|task| task.executor_binding.as_ref())
            .and_then(|binding| binding.model_selection.as_ref())
        {
            let store = settings_store.ok_or_else(|| "恢复子代理缺少模型连接配置".to_string())?;
            let config = crate::model_config::frozen_agent_model_config(store, selection)?;
            return config
                .to_http_model_client()
                .map(|client| Arc::new(client) as Arc<dyn ModelBridgeClient>)
                .ok_or_else(|| "子代理原模型连接不可用".to_string());
        }
        let role_id = execution_role_id
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .or_else(|| task.and_then(|task| task_role_id(Some(task))));

        let thread_id = task
            .map(|task| task.task_id.as_str().to_string())
            .unwrap_or_else(|| ORCHESTRATOR_THREAD_ID.to_string());

        // 有 role_id 时走 RoleTarget::Agent，让 resolve_target_for_role 统一处理:
        //   - 角色未配置 engineId → Ok(None) → 显式继承 orchestrator
        //   - 角色已配置 engineId → 只使用该角色模型，失败直接暴露
        // 无 role_id（顶层会话调用）或角色未配置 → 直接走 orchestrator。
        if let Some(role_id) = role_id
            && let Some(client) = self.resolve_target_cached(
                settings_store,
                RoleTarget::Agent { role_id },
                session_id,
                &thread_id,
            )?
        {
            return Ok(client);
        }

        self.resolve_target_cached(
            settings_store,
            RoleTarget::Orchestrator,
            session_id,
            &thread_id,
        )?
        .ok_or_else(|| "model bridge client 未配置".to_string())
    }
}

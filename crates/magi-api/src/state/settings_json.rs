use super::*;

impl ApiState {
    pub fn settings_snapshot_json_with_mcp_hydration_and_tool_context(
        &self,
        hydrate_mcp_servers: bool,
        tool_context: &ToolExecutionContext,
    ) -> serde_json::Value {
        let mut snapshot = self.settings_store.public_snapshot();
        normalize_settings_snapshot_sections(&mut snapshot);
        if hydrate_mcp_servers {
            self.enrich_mcp_servers_with_connection_status(&mut snapshot);
        }
        let tool_catalog = self.settings_tool_catalog_json(hydrate_mcp_servers, tool_context);
        let skills_config = public_skills_config_section(object_section(&snapshot, "skillsConfig"));
        let public_mcp_servers = public_mcp_servers_section(&snapshot);
        let audit_ledger = self.audit_usage_ledger_dto();
        // 只统计数量：读锁内直接扫描，不克隆整本账本。
        let safeguard_audit_count = crate::safeguard_audit::visible_count(&self.event_bus);
        serde_json::json!({
            "workerConfigs": object_section(&snapshot, "workers"),
            "orchestratorConfig": object_section(&snapshot, "orchestrator"),
            "auxiliaryConfig": object_section(&snapshot, "auxiliary"),
            "visionConfig": object_section(&snapshot, "vision"),
            "imageGenerationConfig": object_section(&snapshot, "imageGeneration"),
            "modelContextWindows": object_section(&snapshot, "modelContextWindows"),
            "userRulesConfig": object_section(&snapshot, "userRulesConfig"),
            "skillsConfig": skills_config,
            "safeguardConfig": object_section(&snapshot, "safeguardConfig"),
            "safeguardAudit": {
                "auditCount": safeguard_audit_count,
                "persistenceHealthy": audit_ledger.is_persist_healthy,
                "pendingFlush": audit_ledger.pending_flush,
            },
            "repositories": array_section(&snapshot, "repositories"),
            "mcpServers": public_mcp_servers,
            "builtinTools": self.builtin_tools_json(&tool_catalog),
            "capabilityDependencies": self.capability_dependencies_json(&tool_catalog),
            "workerStatuses": object_section(&snapshot, "workerStatuses"),
            "runtimeSettings": runtime_settings_from_snapshot(&snapshot),
            "roleTemplates": role_templates_for_registry(self.agent_role_registry.as_ref()),
            "registryEngines": crate::model_config::load_registry_engines(self),
            "registryAgents": resolve_registry_agents(self),
            "bootstrapScope": if hydrate_mcp_servers { "full" } else { "core" },
            "mcpServersHydrated": hydrate_mcp_servers,
        })
    }

    pub(super) fn settings_tool_catalog_json(
        &self,
        include_external_dependencies: bool,
        tool_context: &ToolExecutionContext,
    ) -> serde_json::Value {
        let input = if include_external_dependencies {
            r#"{"include_external":true,"include_mcp_servers":true,"include_agent_roles":false}"#
        } else {
            r#"{"include_external":false,"include_mcp_servers":false,"include_agent_roles":false}"#
        };
        self.tool_catalog_json(input, tool_context)
            .unwrap_or(serde_json::Value::Null)
    }

    pub(crate) fn tool_catalog_json(
        &self,
        input: &str,
        tool_context: &ToolExecutionContext,
    ) -> Result<serde_json::Value, ApiError> {
        let Some(registry) = &self.tool_registry else {
            return Err(ApiError::not_found("工具注册表未配置", "tool_registry"));
        };
        Ok(registry.tool_catalog_value(input, tool_context))
    }

    pub(crate) fn public_tool_catalog_json(
        &self,
        input: &str,
        tool_context: &ToolExecutionContext,
    ) -> Result<serde_json::Value, ApiError> {
        self.tool_catalog_json(input, tool_context)
            .map(|catalog| public_tool_catalog_response_json(&catalog))
    }

    pub(super) fn builtin_tools_json(&self, tool_catalog: &serde_json::Value) -> serde_json::Value {
        if tool_catalog.is_null() {
            return serde_json::Value::Array(Vec::new());
        }
        let tools = tool_catalog
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|tool| tool.get("public").and_then(serde_json::Value::as_bool) == Some(true))
            .map(|tool| {
                serde_json::json!({
                    "name": tool.get("name").cloned().unwrap_or(serde_json::Value::Null),
                    "category": tool.get("category").cloned().unwrap_or(serde_json::Value::String("uncategorized".to_string())),
                    "riskLevel": tool.get("risk_level").cloned().unwrap_or(serde_json::Value::String("low".to_string())),
                    "approvalRequirement": tool.get("approval_requirement").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "effectiveApprovalPolicy": tool.get("effective_approval_policy").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "accessProfileBehavior": tool.get("access_profile_behavior").cloned().unwrap_or(serde_json::Value::String("restricted_allowed".to_string())),
                    "accessMode": tool.get("access_mode").cloned().unwrap_or(serde_json::Value::String("read_only".to_string())),
                    "policyScope": tool.get("policy_scope").cloned().unwrap_or(serde_json::Value::String("fixed".to_string())),
                    "modelCallScope": tool.get("model_call_scope").cloned().unwrap_or(serde_json::Value::String("session_or_task".to_string())),
                    "workspaceRequired": tool.get("workspace_required").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "modelAvailable": tool.get("model_available").cloned().unwrap_or(serde_json::Value::Bool(true)),
                    "inputSensitivePolicy": tool.get("input_sensitive_policy").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "policySummary": tool.get("policy_summary").cloned().unwrap_or(serde_json::Value::String("使用工具默认风险策略".to_string())),
                    "runtimeInternal": tool.get("runtime_internal").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "runtimeStatus": normalize_tool_runtime_status(tool.get("runtime_status")),
                    "runtimeWarnings": warning_markers(tool, "runtime_warnings", "runtime_warning"),
                    "schemaStatus": tool.get("schema_status").cloned().unwrap_or(serde_json::Value::String("ok".to_string())),
                    "schemaWarnings": warning_markers(tool, "schema_warnings", "schema_warning"),
                    "enabled": true,
                })
            })
            .collect::<Vec<_>>();
        serde_json::Value::Array(tools)
    }

    pub(super) fn capability_dependencies_json(
        &self,
        tool_catalog: &serde_json::Value,
    ) -> serde_json::Value {
        let dependencies = tool_catalog
            .get("runtime_dependencies")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(normalize_capability_dependency_json)
            .collect::<Vec<_>>();
        serde_json::Value::Array(dependencies)
    }

    pub fn settings_runtime_json(&self) -> serde_json::Value {
        let snapshot = self.settings_store.public_snapshot();
        runtime_settings_from_snapshot(&snapshot)
    }

    pub fn runtime_status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": "running",
            "version": self.service_info.api_version,
        })
    }

    pub(super) fn enrich_mcp_servers_with_connection_status(
        &self,
        snapshot: &mut HashMap<String, serde_json::Value>,
    ) {
        let Some(servers) = snapshot.get_mut("mcpServers") else {
            return;
        };
        let Some(arr) = servers.as_array_mut() else {
            return;
        };
        for entry in arr.iter_mut() {
            let Some(server_id) = mcp_server_entry_id(entry).map(str::to_string) else {
                continue;
            };
            let enabled = mcp_server_entry_enabled(entry);

            if !enabled {
                let mut pool = self
                    .mcp_connections
                    .write()
                    .expect("mcp connections write lock poisoned");
                pool.remove(&server_id);
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disabled");
                entry.as_object_mut().map(|m| m.remove("error"));
                continue;
            }

            let already_connected = {
                let pool = self
                    .mcp_connections
                    .read()
                    .expect("mcp connections read lock poisoned");
                pool.contains_key(&server_id)
            };

            if already_connected {
                entry["connected"] = serde_json::json!(true);
                entry["health"] = serde_json::json!("connected");
                entry.as_object_mut().map(|m| m.remove("error"));
            } else if build_mcp_config_from_entry(entry).is_some() {
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disconnected");
            } else {
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disconnected");
                entry["error"] = serde_json::json!("mcp_invalid_config");
            }
        }
    }
}

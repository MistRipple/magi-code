use super::*;

impl BrowserToolRuntimeDependencies {
    pub(super) async fn execute_devtools_operation(
        &self,
        tool_name: &str,
        arguments: &Map<String, Value>,
        browser_session: &magi_browser_authority::BrowserSession,
        tab: &magi_browser_authority::BrowserTab,
        scope: BrowserToolCallScope<'_>,
        client: &BrowserHostClient,
    ) -> Result<String, BrowserToolError> {
        let operation = browser_devtools_operation(tool_name).ok_or_else(|| {
            BrowserToolError::new("unknown_browser_tool", "未知的浏览器 DevTools 工具")
        })?;
        validate_devtools_arguments(operation, arguments)?;
        let action = optional_string(arguments, "action");
        let lighthouse_mode = optional_string(arguments, "mode");
        let requires_write = match operation {
            "hover" | "drag" | "fill_form" | "upload_file" | "click_at" | "evaluate"
            | "emulate" => true,
            "dialog" => !matches!(action.as_deref(), Some("list")),
            "console" | "network" => matches!(action.as_deref(), Some("clear")),
            "performance" => matches!(
                action.as_deref(),
                Some(
                    "start"
                        | "stop"
                        | "profile_start"
                        | "profile_stop"
                        | "coverage_start"
                        | "coverage_stop"
                )
            ),
            "lighthouse" => !matches!(lighthouse_mode.as_deref(), Some("snapshot")),
            "heap" => matches!(action.as_deref(), Some("take_snapshot" | "close_snapshot")),
            "third_party" => matches!(action.as_deref(), Some("clear")),
            "webmcp" => matches!(action.as_deref(), Some("execute")),
            "storage" => matches!(action.as_deref(), Some("set" | "remove" | "clear")),
            "pwa" => false,
            _ => false,
        };
        let control = if requires_write {
            Some(
                self.prepare_agent_write(client, browser_session, tab, scope)
                    .await?,
            )
        } else {
            None
        };
        // 上传的文件路径已由工具调用前的访问档位策略授权（与 file_read 同一套路径范围），
        // 这里只把它解析成 Worker 可直接使用的绝对路径。
        let host_arguments = if operation == "upload_file" {
            resolve_upload_file_arguments(arguments, scope.context)?
        } else {
            arguments.clone()
        };
        let reply = client
            .request(BrowserHostCommand::Devtools {
                tab_id: tab.tab_id.clone(),
                control,
                operation: operation.to_string(),
                arguments: Value::Object(host_arguments),
            })
            .await
            .map_err(browser_host_client_error)?;
        let BrowserHostCommandResult::Json { value } =
            succeeded_result(reply.response.outcome, "浏览器 DevTools 工具执行失败")?
        else {
            return Err(BrowserToolError::new(
                "browser_result_invalid",
                "浏览器 DevTools 工具返回结果无效",
            ));
        };
        let snapshot = if bool_arg(arguments, "include_snapshot", false) {
            Some(browser_tool_snapshot_value(
                &self.capture_snapshot(client, &tab.tab_id, None).await?,
                &tab.tab_id,
            ))
        } else {
            None
        };
        Ok(json!({
            "tool": tool_name,
            "status": "succeeded",
            "result": value,
            "snapshot": snapshot,
        })
        .to_string())
    }

    pub(super) async fn execute_tabs(
        &self,
        arguments: &Map<String, Value>,
        session: &magi_browser_authority::BrowserSession,
        scope: BrowserToolCallScope<'_>,
        client: &BrowserHostClient,
    ) -> Result<String, BrowserToolError> {
        validate_browser_tabs_arguments(arguments)?;
        let action = string_arg(arguments, "action")?;
        if action == "list" {
            let authority = self
                .authority
                .lock()
                .expect("browser authority lock poisoned");
            let tabs = session
                .tab_ids
                .iter()
                .filter_map(|id| authority.tab(id))
                .cloned()
                .collect::<Vec<_>>();
            return Ok(json!({
                "tool": "browser_tabs",
                "status": "succeeded",
                "active_tab_id": authority.active_tab(&session.browser_session_id),
                "tabs": tabs,
            })
            .to_string());
        }
        match action.as_str() {
            "new" => {
                let initial_url =
                    optional_string(arguments, "url").unwrap_or_else(|| "about:blank".to_string());
                validate_browser_navigation_url_with(
                    &initial_url,
                    self.capabilities().lan_access_enabled,
                )
                .map_err(|error| {
                    BrowserToolError::new(
                        "browser_navigation_url_rejected",
                        format!("浏览器导航 URL 不合法: {error}"),
                    )
                })?;
                let tab_id = BrowserTabId::new(format!(
                    "browser-tool-tab-{}-{}",
                    UtcMillis::now().0,
                    BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
                let created = self.mutate(|authority| {
                    let created = authority.create_tab(CreateBrowserTab {
                        tab_id: tab_id.clone(),
                        browser_session_id: session.browser_session_id.clone(),
                        url: initial_url.clone(),
                        now: UtcMillis::now(),
                    })?;
                    authority.set_active_tab(&session.browser_session_id, &tab_id)?;
                    Ok(created)
                })?;
                self.publish_tab_event("browser.tab.created", &created);
                let reply = match client
                    .request(BrowserHostCommand::CreatePage {
                        tab_id: tab_id.clone(),
                        browser_session_id: session.browser_session_id.clone(),
                        initial_url,
                        logical_viewport: magi_browser_authority::BrowserLogicalViewport::Auto,
                        navigation_revision: 0,
                        snapshot_revision: 0,
                        allow_page_eviction: false,
                    })
                    .await
                {
                    Ok(reply) => reply,
                    Err(error) => {
                        self.mark_tab_crashed(&tab_id);
                        return Err(BrowserToolError::new(
                            "browser_host_disconnected",
                            error.to_string(),
                        ));
                    }
                };
                let page = match page_state(reply.response.outcome, "创建浏览器 Tab 失败") {
                    Ok(page) => page,
                    Err(error) => {
                        self.mark_tab_crashed(&tab_id);
                        return Err(error);
                    }
                };
                let created = self.apply_page_state(&tab_id, page)?;
                self.publish_tab_event("browser.tab.created", &created);
                Ok(
                    json!({ "tool": "browser_tabs", "status": "succeeded", "tab": created })
                        .to_string(),
                )
            }
            "activate" => {
                let tab_id = BrowserTabId::new(string_arg(arguments, "tab_id")?);
                let tab = tab_in_session(self, session, &tab_id)?;
                // 与 HTTP 激活入口保持同一顺序：先通知 App Renderer 切换右栏
                // 的真实内容槽，再让 Host 恢复页面。否则 RestorePage 会在
                // Renderer 仍显示旧 Tab 时等待新 Tab 的槽位，工具调用会卡住。
                self.publish_browser_event(
                    "browser.tab.activation_requested",
                    session,
                    json!({
                        "browser_session_id": session.browser_session_id,
                        "tab_id": tab_id,
                    }),
                );
                let target = self.materialize_tab(tab, client).await?;
                self.ensure_surface(client, &target.tab_id).await?;
                self.prepare_agent_write(client, session, &target, scope)
                    .await?;
                self.mutate(|authority| {
                    authority.set_active_tab(&session.browser_session_id, &tab_id)
                })?;
                self.publish_browser_event(
                    "browser.tab.activated",
                    session,
                    json!({
                        "browser_session_id": session.browser_session_id,
                        "tab_id": tab_id,
                    }),
                );
                Ok(
                    json!({ "tool": "browser_tabs", "status": "succeeded", "tab_id": tab_id })
                        .to_string(),
                )
            }
            "close" => {
                let tab_id = BrowserTabId::new(string_arg(arguments, "tab_id")?);
                let _target = tab_in_session(self, session, &tab_id)?;
                let closed = self.mutate(|authority| {
                    authority.transition_tab(
                        &tab_id,
                        magi_browser_authority::BrowserTabLifecycle::Closed,
                        UtcMillis::now(),
                    )
                })?;
                self.publish_tab_event("browser.tab.closed", &closed);
                // 权威关闭事件必须先到达 App Renderer，使其卸载
                // <webview>；Host 只负责随后收口物理 guest，不能在
                // Renderer 仍持有 guest 时直接 close WebContents。
                let close_result = client
                    .request(BrowserHostCommand::ClosePage {
                        tab_id: tab_id.clone(),
                    })
                    .await;
                if let Err(error) = close_result {
                    return Err(BrowserToolError::new(
                        "browser_host_disconnected",
                        error.to_string(),
                    ));
                }
                Ok(json!({ "tool": "browser_tabs", "status": "succeeded", "closed_tab_id": tab_id }).to_string())
            }
            _ => Err(BrowserToolError::new(
                "invalid_tabs_action",
                "browser_tabs action 不合法",
            )),
        }
    }
}

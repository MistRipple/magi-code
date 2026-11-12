use super::*;

impl BrowserToolRuntimeDependencies {
    pub(super) async fn capture_snapshot(
        &self,
        client: &BrowserHostClient,
        tab_id: &BrowserTabId,
        selector: Option<String>,
    ) -> Result<magi_browser_authority::BrowserHostSnapshot, BrowserToolError> {
        let (navigation_revision, snapshot_revision) =
            self.mutate(|authority| authority.record_snapshot(tab_id, UtcMillis::now()))?;
        let reply = client
            .request(BrowserHostCommand::Snapshot {
                tab_id: tab_id.clone(),
                navigation_revision,
                snapshot_revision,
                limits: Default::default(),
                selector,
            })
            .await
            .map_err(browser_host_client_error)?;
        let BrowserHostCommandResult::Snapshot(snapshot) =
            succeeded_result(reply.response.outcome, "浏览器快照失败")?
        else {
            return Err(BrowserToolError::new(
                "browser_snapshot_failed",
                "浏览器快照结果无效",
            ));
        };
        if snapshot.tab_id != *tab_id {
            return Err(BrowserToolError::new(
                "browser_snapshot_tab_mismatch",
                "浏览器快照返回了错误的 Tab",
            ));
        }
        self.mutate(|authority| {
            authority.validate_snapshot_result(
                tab_id,
                snapshot.navigation_revision,
                snapshot.snapshot_revision,
            )
        })?;
        Ok(snapshot)
    }

    pub(super) fn persist_artifact(
        &self,
        session_id: &SessionId,
        _call_id: &str,
        bytes: &[u8],
        extension: &str,
    ) -> Result<String, BrowserToolError> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Err(BrowserToolError::new(
                "browser_artifact_store_unavailable",
                "浏览器 artifact 存储不可用",
            ));
        };
        let root = persistence
            .state_root()
            .ok_or_else(|| {
                BrowserToolError::new(
                    "browser_artifact_store_unavailable",
                    "浏览器 artifact 根目录不可用",
                )
            })?
            .join(BROWSER_ARTIFACT_DIR)
            .join(session_id.as_str());
        std::fs::create_dir_all(&root).map_err(|error| {
            BrowserToolError::new("browser_artifact_write_failed", error.to_string())
        })?;
        let path = root.join(format!(
            "browser-shot-{}-{}.{}",
            UtcMillis::now().0,
            BROWSER_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            extension,
        ));
        magi_core::fs_atomic::write_atomic(&path, bytes).map_err(|error| {
            BrowserToolError::new("browser_artifact_write_failed", error.to_string())
        })?;
        if let Some(artifact_root) = root.parent() {
            prune_browser_artifacts(artifact_root, MAX_BROWSER_ARTIFACT_BYTES);
        }
        Ok(path.display().to_string())
    }

    /// 列出本会话的下载，或把已完成的下载复制进工作区。
    ///
    /// 目标路径已在工具调用前按访问档位授权（与 file_write 同一套路径范围）；这里只负责
    /// 解析路径与原子地复制。下载文件在 Desktop 私有目录里的位置从不返回给模型。
    pub(super) async fn execute_download(
        &self,
        arguments: &Map<String, Value>,
        session: &magi_browser_authority::BrowserSession,
        scope: BrowserToolCallScope<'_>,
    ) -> Result<String, BrowserToolError> {
        let action = string_arg(arguments, "action")?;
        let tab_ids = session.tab_ids.clone();
        match action.as_str() {
            "list" => {
                let filter = optional_string(arguments, "tab_id").map(BrowserTabId::new);
                if let Some(tab_id) = filter.as_ref()
                    && !tab_ids.contains(tab_id)
                {
                    return Err(BrowserToolError::new(
                        "browser_tab_scope_mismatch",
                        "指定的浏览器 Tab 不属于当前浏览器会话",
                    ));
                }
                let scoped = filter.map_or_else(|| tab_ids.clone(), |tab_id| vec![tab_id]);
                Ok(json!({
                    "tool": "browser_download",
                    "status": "succeeded",
                    "downloads": self.downloads.list_for_tabs(&scoped),
                })
                .to_string())
            }
            "save" => {
                let download_id = string_arg(arguments, "download_id")?;
                let destination = string_arg(arguments, "destination_path")?;
                let overwrite = bool_arg(arguments, "overwrite", false);
                let Some(state) = self.downloads.download_state(&download_id, &tab_ids) else {
                    return Err(BrowserToolError::new(
                        "browser_download_not_found",
                        "当前浏览器会话里没有这个下载；先用 action=list 查看 download_id",
                    ));
                };
                let Some((source, filename)) =
                    self.downloads.completed_file(&download_id, &tab_ids)
                else {
                    return Err(BrowserToolError::new(
                        "browser_download_not_ready",
                        format!("下载尚未完成（当前状态 {state}）；等待完成后再保存"),
                    ));
                };
                let working_directory =
                    scope.context.working_directory.as_deref().ok_or_else(|| {
                        BrowserToolError::new(
                            "browser_download_working_directory_unavailable",
                            "当前任务没有工作目录，无法解析保存位置",
                        )
                    })?;
                let mut target = magi_core::HostPath::resolve_native_input(
                    &destination,
                    Some(working_directory),
                    dirs::home_dir().as_deref(),
                )
                .map(magi_core::HostPath::into_path_buf)
                .map_err(|_| BrowserToolError::new("invalid_arguments", "destination_path 无效"))?;
                if target.is_dir() {
                    target.push(&filename);
                }
                let bytes = copy_download_into_place(&source, &target, overwrite).await?;
                Ok(json!({
                    "tool": "browser_download",
                    "status": "succeeded",
                    "saved": true,
                    "download_id": download_id,
                    "filename": filename,
                    "destination_path": destination,
                    "bytes": bytes,
                })
                .to_string())
            }
            _ => Err(BrowserToolError::new(
                "invalid_arguments",
                "browser_download 的 action 只支持 list 或 save",
            )),
        }
    }
}

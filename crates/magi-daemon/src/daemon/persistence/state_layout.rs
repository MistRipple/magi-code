use super::*;

impl StateRepository {
    /// 启动时恢复中断的 session projection 事务，并校验 state layout 版本标记。
    ///
    /// 只接受当前版本布局：标记版本不一致直接拒绝启动；没有标记的 state 目录视为
    /// 全新状态并写入当前版本标记。
    pub(crate) fn verify_state_layout(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        self.recover_session_projection_transaction(workspace_roots)?;
        let layout_path = self.state_layout_marker_path();
        if layout_path.exists() {
            let marker: StateLayoutMarker = self.read_json_strict(&layout_path)?;
            if matches!(marker.version, 2..=20) {
                // 升级写屏障，防止旧 daemon 重写新 Agent activation 与 Goal/Step。
                let backup = self
                    .state_root
                    .join(format!("state-layout.v{}.json", marker.version));
                if !backup.exists() {
                    self.write_json_atomically(backup, &marker)?;
                }
                return self.write_json_atomically(
                    layout_path,
                    &StateLayoutMarker {
                        version: STATE_LAYOUT_VERSION,
                    },
                );
            }
            return self.validate_state_layout_marker(&layout_path);
        }
        self.write_json_atomically(
            layout_path,
            &StateLayoutMarker {
                version: STATE_LAYOUT_VERSION,
            },
        )
    }

    pub(super) fn state_layout_marker_path(&self) -> PathBuf {
        self.state_root.join("state-layout.json")
    }

    pub(super) fn validate_state_layout_marker(
        &self,
        layout_path: &Path,
    ) -> Result<(), DaemonError> {
        let marker: StateLayoutMarker = self.read_json_strict(layout_path)?;
        if marker.version != STATE_LAYOUT_VERSION {
            return Err(DaemonError::internal(format!(
                "不支持的 state layout 版本: {}",
                marker.version
            )));
        }
        Ok(())
    }

    pub(super) fn ensure_state_layout_marker_locked(&self) -> Result<(), DaemonError> {
        let layout_path = self.state_layout_marker_path();
        if layout_path.exists() {
            return self.validate_state_layout_marker(&layout_path);
        }
        self.write_json_atomically_locked(
            layout_path,
            &StateLayoutMarker {
                version: STATE_LAYOUT_VERSION,
            },
        )
    }
}

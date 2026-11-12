use super::*;

impl StateRepository {
    pub(super) fn session_projection_transaction_path(&self) -> PathBuf {
        self.state_root.join("session-projection-transaction.json")
    }

    pub(super) fn recover_session_projection_transaction(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        let path = self.session_projection_transaction_path();
        if !path.exists() {
            return Ok(());
        }
        let roots = workspace_roots.iter().cloned().collect::<HashMap<_, _>>();
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let transaction: SessionProjectionTransaction = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|error| {
            DaemonError::internal(format!(
                "session projection transaction 损坏，拒绝继续启动 {}: {error}",
                path.display()
            ))
        })?;
        self.apply_session_projection_transaction_locked(&transaction, &roots)
    }

    pub(super) fn commit_session_projection_transaction_locked(
        &self,
        transaction: &SessionProjectionTransaction,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        if transaction.writes.is_empty() && transaction.removals.is_empty() {
            return Ok(());
        }
        self.validate_session_projection_transaction(transaction, workspace_roots)?;
        let path = self.session_projection_transaction_path();
        fs::create_dir_all(&self.state_root)?;
        magi_core::fs_atomic::write_atomic(
            &path,
            serde_json::to_vec_pretty(transaction).map_err(DaemonError::from)?,
        )?;
        self.apply_session_projection_transaction_locked(transaction, workspace_roots)
    }

    pub(super) fn apply_session_projection_transaction_locked(
        &self,
        transaction: &SessionProjectionTransaction,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        self.validate_session_projection_transaction(transaction, workspace_roots)?;
        for write in &transaction.writes {
            if let Some(parent) = write.path.parent() {
                fs::create_dir_all(parent)?;
            }
            magi_core::fs_atomic::write_atomic(&write.path, write.content.as_bytes())?;
        }
        for removal in &transaction.removals {
            match removal.kind {
                SessionProjectionRemovalKind::File => {
                    Self::remove_file_durable(&removal.path)?;
                }
                SessionProjectionRemovalKind::Directory => {
                    Self::remove_directory_durable(&removal.path)?;
                }
            }
        }
        Self::remove_file_durable(&self.session_projection_transaction_path())
    }

    pub(super) fn validate_session_projection_transaction(
        &self,
        transaction: &SessionProjectionTransaction,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        if transaction.schema_version != SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION
            || transaction.transaction_id.trim().is_empty()
        {
            return Err(DaemonError::internal(
                "session projection transaction schema 或 transactionId 无效".to_string(),
            ));
        }
        let workspace_state_roots = workspace_roots
            .values()
            .map(|root| root.join(".magi"))
            .collect::<Vec<_>>();
        let allowed = |path: &Path| {
            path.starts_with(&self.state_root)
                || workspace_state_roots
                    .iter()
                    .any(|root| path.starts_with(root))
        };
        let mut write_paths = HashSet::new();
        for write in &transaction.writes {
            if !allowed(&write.path) || !write_paths.insert(write.path.clone()) {
                return Err(DaemonError::internal(format!(
                    "session projection transaction 包含越界或重复写路径: {}",
                    write.path.display()
                )));
            }
        }
        let mut removal_paths = HashSet::new();
        for removal in &transaction.removals {
            if !allowed(&removal.path)
                || write_paths.contains(&removal.path)
                || !removal_paths.insert(removal.path.clone())
                || removal.path == self.state_root
                || workspace_state_roots
                    .iter()
                    .any(|root| &removal.path == root)
            {
                return Err(DaemonError::internal(format!(
                    "session projection transaction 包含越界、冲突或重复删除路径: {}",
                    removal.path.display()
                )));
            }
        }
        Ok(())
    }

    pub(super) fn sync_parent_directory(path: &Path) {
        if let Some(parent) = path.parent()
            && let Ok(directory) = fs::File::open(parent)
        {
            let _ = directory.sync_all();
        }
    }

    pub(super) fn remove_file_durable(path: &Path) -> Result<(), DaemonError> {
        match fs::remove_file(path) {
            Ok(()) => Self::sync_parent_directory(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub(super) fn remove_directory_durable(path: &Path) -> Result<(), DaemonError> {
        match fs::remove_dir_all(path) {
            Ok(()) => Self::sync_parent_directory(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

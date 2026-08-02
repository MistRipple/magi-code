use crate::assets::{AppearanceAssetStore, StoredAsset};
use crate::builtin::{builtin_assets, builtin_themes};
use crate::contract::*;
use crate::package::{export_theme_package, import_theme_package};
use crate::validator::{editable_for_source, theme_content_hash, validate_user_theme};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_FILE: &str = "state.json";
const STAGED_ASSET_TTL_MILLIS: u64 = 24 * 60 * 60 * 1_000;
static TEMP_LIBRARY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportConflictStrategy {
    Reject,
    Duplicate,
    Replace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppearanceErrorKind {
    InvalidInput,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearanceError {
    kind: AppearanceErrorKind,
    message: String,
}

impl AppearanceError {
    pub fn kind(&self) -> AppearanceErrorKind {
        self.kind
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: AppearanceErrorKind::InvalidInput,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            kind: AppearanceErrorKind::Conflict,
            message: message.into(),
        }
    }
}

impl fmt::Display for AppearanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppearanceError {}

impl From<String> for AppearanceError {
    fn from(message: String) -> Self {
        Self::invalid(message)
    }
}

#[derive(Debug)]
pub struct AppearanceLibrary {
    root: PathBuf,
    state: RwLock<PersistedAppearanceState>,
    assets: AppearanceAssetStore,
    temporary: bool,
}

impl AppearanceLibrary {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| format!("创建外观目录失败: {error}"))?;
        let assets = AppearanceAssetStore::new(root.join("assets"))?;
        for builtin_asset in builtin_assets() {
            let stored = assets.put_image(builtin_asset.bytes)?;
            if stored.asset_id != builtin_asset.asset_id {
                return Err("内置主题资源完整性校验失败".to_string());
            }
        }
        let state_path = root.join(STATE_FILE);
        let state = if state_path.exists() {
            let bytes =
                fs::read(&state_path).map_err(|error| format!("读取外观状态失败: {error}"))?;
            let state: PersistedAppearanceState = serde_json::from_slice(&bytes)
                .map_err(|error| format!("解析外观状态失败: {error}"))?;
            for record in &state.user_themes {
                validate_user_theme(&record.pack)?;
            }
            state
        } else {
            PersistedAppearanceState {
                revision: 1,
                active_theme_id: "builtin.system".to_string(),
                user_themes: Vec::new(),
                staged_assets: Default::default(),
            }
        };
        let library = Self {
            root,
            state: RwLock::new(state),
            assets,
            temporary: false,
        };
        if !state_path.exists() {
            library.persist()?;
        }
        if !library.theme_exists(&library.state.read().unwrap().active_theme_id) {
            return Err("当前主题不存在，外观状态无法恢复".to_string());
        }
        library.cleanup_assets()?;
        Ok(library)
    }

    pub fn in_memory() -> Self {
        let root = std::env::temp_dir().join(format!(
            "magi-appearance-memory-{}-{}-{}",
            std::process::id(),
            now_millis(),
            TEMP_LIBRARY_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let mut library = Self::open(root).expect("临时外观目录必须可创建");
        library.temporary = true;
        library
    }

    pub fn snapshot(&self) -> AppearanceSnapshot {
        let state = self
            .state
            .read()
            .expect("appearance state read lock poisoned");
        let mut themes = builtin_themes();
        themes.extend(state.user_themes.clone());
        AppearanceSnapshot {
            revision: state.revision,
            active_theme_id: state.active_theme_id.clone(),
            themes,
        }
    }

    pub fn put_asset(&self, bytes: &[u8]) -> Result<StoredAsset, AppearanceError> {
        let asset = self.assets.put_image(bytes)?;
        let mut state = self
            .state
            .write()
            .expect("appearance state write lock poisoned");
        let previous = state.clone();
        state
            .staged_assets
            .insert(asset.asset_id.clone(), now_millis());
        if let Err(error) = self.persist_state(&state) {
            *state = previous;
            return Err(error.into());
        }
        Ok(asset)
    }

    pub fn read_asset(&self, asset_id: &str) -> Result<(Vec<u8>, String), AppearanceError> {
        self.assets.read(asset_id).map_err(Into::into)
    }

    pub fn create_theme(
        &self,
        pack: ThemePack,
        expected_revision: u64,
        source: ThemeSource,
    ) -> Result<AppearanceSnapshot, AppearanceError> {
        if source == ThemeSource::Builtin {
            return Err(AppearanceError::invalid("不能创建内置主题"));
        }
        validate_user_theme(&pack)?;
        self.ensure_assets_exist(&pack)?;
        self.mutate(expected_revision, |state| {
            if state
                .user_themes
                .iter()
                .any(|record| record.pack.id == pack.id)
                || builtin_themes()
                    .iter()
                    .any(|record| record.pack.id == pack.id)
            {
                return Err("主题标识已存在".to_string());
            }
            let timestamp = now_millis();
            state.user_themes.push(ThemeRecord {
                content_hash: theme_content_hash(&pack)?,
                pack,
                source,
                editable: editable_for_source(source),
                revision: 1,
                created_at: timestamp,
                updated_at: timestamp,
            });
            if let Some(wallpaper) = &state
                .user_themes
                .last()
                .expect("刚插入的主题必须存在")
                .pack
                .wallpaper
            {
                state.staged_assets.remove(&wallpaper.asset_id);
            }
            Ok(())
        })?;
        self.cleanup_assets().map_err(AppearanceError::invalid)?;
        Ok(self.snapshot())
    }

    pub fn update_theme(
        &self,
        theme_id: &str,
        pack: ThemePack,
        expected_revision: u64,
    ) -> Result<AppearanceSnapshot, AppearanceError> {
        if theme_id.starts_with("builtin.") {
            return Err(AppearanceError::invalid(
                "内置主题不可编辑，请基于它新建主题",
            ));
        }
        if pack.id != theme_id {
            return Err(AppearanceError::invalid("编辑主题时不能改变主题标识"));
        }
        validate_user_theme(&pack)?;
        self.ensure_assets_exist(&pack)?;
        self.mutate(expected_revision, |state| {
            let record = state
                .user_themes
                .iter_mut()
                .find(|record| record.pack.id == theme_id)
                .ok_or_else(|| "主题不存在".to_string())?;
            record.content_hash = theme_content_hash(&pack)?;
            record.pack = pack;
            record.revision = record.revision.saturating_add(1);
            record.updated_at = now_millis();
            if let Some(wallpaper) = &record.pack.wallpaper {
                state.staged_assets.remove(&wallpaper.asset_id);
            }
            Ok(())
        })?;
        self.cleanup_assets().map_err(AppearanceError::invalid)?;
        Ok(self.snapshot())
    }

    pub fn activate(
        &self,
        theme_id: &str,
        expected_revision: u64,
    ) -> Result<AppearanceSnapshot, AppearanceError> {
        if !self.theme_exists(theme_id) {
            return Err(AppearanceError::invalid("主题不存在"));
        }
        self.mutate(expected_revision, |state| {
            state.active_theme_id = theme_id.to_string();
            Ok(())
        })?;
        Ok(self.snapshot())
    }

    pub fn delete_theme(
        &self,
        theme_id: &str,
        expected_revision: u64,
    ) -> Result<AppearanceSnapshot, AppearanceError> {
        if theme_id.starts_with("builtin.") {
            return Err(AppearanceError::invalid("内置主题不可删除"));
        }
        self.mutate(expected_revision, |state| {
            if state.active_theme_id == theme_id {
                return Err("当前使用中的主题不能删除，请先切换其他主题".to_string());
            }
            let before = state.user_themes.len();
            state
                .user_themes
                .retain(|record| record.pack.id != theme_id);
            if before == state.user_themes.len() {
                return Err("主题不存在".to_string());
            }
            Ok(())
        })?;
        self.cleanup_assets().map_err(AppearanceError::invalid)?;
        Ok(self.snapshot())
    }

    pub fn import_theme(
        &self,
        bytes: &[u8],
        expected_revision: u64,
        strategy: ImportConflictStrategy,
    ) -> Result<AppearanceSnapshot, AppearanceError> {
        let mut imported = import_theme_package(bytes)?;
        let existing = self
            .snapshot()
            .themes
            .into_iter()
            .find(|record| record.pack.id == imported.pack.id);
        if let Some(existing) = existing {
            let content_matches = existing.content_hash == theme_content_hash(&imported.pack)?;
            match strategy {
                ImportConflictStrategy::Reject => {
                    return Err(AppearanceError::invalid(if content_matches {
                        "该主题已经存在"
                    } else {
                        "主题标识冲突，请选择导入为新主题或替换"
                    }));
                }
                ImportConflictStrategy::Duplicate => {
                    imported.pack.id = format!(
                        "user.imported-{}-{}",
                        now_millis(),
                        TEMP_LIBRARY_COUNTER.fetch_add(1, Ordering::Relaxed),
                    );
                }
                ImportConflictStrategy::Replace => {
                    if existing.source == ThemeSource::Builtin {
                        return Err(AppearanceError::invalid("不能替换内置主题，请导入为新主题"));
                    }
                    if content_matches {
                        return Ok(self.snapshot());
                    }
                    for asset in &imported.assets {
                        self.put_asset(asset)?;
                    }
                    return self.update_theme(&existing.pack.id, imported.pack, expected_revision);
                }
            }
        }
        for asset in &imported.assets {
            self.put_asset(asset)?;
        }
        self.create_theme(imported.pack, expected_revision, ThemeSource::Imported)
    }

    pub fn export_theme(&self, theme_id: &str) -> Result<Vec<u8>, AppearanceError> {
        let record = self
            .state
            .read()
            .unwrap()
            .user_themes
            .iter()
            .find(|record| record.pack.id == theme_id)
            .cloned()
            .ok_or_else(|| AppearanceError::invalid("仅用户创建或导入的主题支持导出"))?;
        export_theme_package(&record.pack, &self.assets).map_err(Into::into)
    }

    fn mutate(
        &self,
        expected_revision: u64,
        operation: impl FnOnce(&mut PersistedAppearanceState) -> Result<(), String>,
    ) -> Result<(), AppearanceError> {
        let mut state = self
            .state
            .write()
            .expect("appearance state write lock poisoned");
        if state.revision != expected_revision {
            return Err(AppearanceError::conflict(
                "外观配置已在其他窗口更新，请刷新后重试",
            ));
        }
        let previous = state.clone();
        operation(&mut state).map_err(AppearanceError::invalid)?;
        state.revision = state.revision.saturating_add(1);
        if let Err(error) = self.persist_state(&state) {
            *state = previous;
            return Err(AppearanceError::invalid(error));
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), String> {
        self.persist_state(&self.state.read().unwrap())
    }

    fn persist_state(&self, state: &PersistedAppearanceState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("序列化外观状态失败: {error}"))?;
        magi_core::fs_atomic::write_atomic(&self.root.join(STATE_FILE), bytes)
            .map_err(|error| format!("保存外观状态失败: {error}"))
    }

    fn theme_exists(&self, theme_id: &str) -> bool {
        builtin_themes()
            .iter()
            .any(|record| record.pack.id == theme_id)
            || self
                .state
                .read()
                .unwrap()
                .user_themes
                .iter()
                .any(|record| record.pack.id == theme_id)
    }

    fn ensure_assets_exist(&self, pack: &ThemePack) -> Result<(), String> {
        if let Some(wallpaper) = &pack.wallpaper
            && self.assets.resolve(&wallpaper.asset_id).is_none()
        {
            return Err("主题引用的背景图不存在".to_string());
        }
        Ok(())
    }

    fn cleanup_assets(&self) -> Result<(), String> {
        let now = now_millis();
        let mut state = self
            .state
            .write()
            .expect("appearance state write lock poisoned");
        let referenced = builtin_themes()
            .into_iter()
            .chain(state.user_themes.iter().cloned())
            .filter_map(|record| record.pack.wallpaper.map(|wallpaper| wallpaper.asset_id))
            .collect::<HashSet<_>>();
        let staged_before = state.staged_assets.len();
        state.staged_assets.retain(|asset_id, staged_at| {
            referenced.contains(asset_id)
                || now.saturating_sub(*staged_at) < STAGED_ASSET_TTL_MILLIS
        });
        if state.staged_assets.len() != staged_before {
            self.persist_state(&state)?;
        }
        let protected = referenced
            .into_iter()
            .chain(state.staged_assets.keys().cloned())
            .collect::<HashSet<_>>();
        drop(state);
        self.assets.cleanup_unreferenced(&protected)
    }
}

impl Drop for AppearanceLibrary {
    fn drop(&mut self) {
        if self.temporary {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

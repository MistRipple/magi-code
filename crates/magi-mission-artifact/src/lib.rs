use magi_core::{MissionId, WorkspaceRootPath};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

#[derive(Debug)]
pub struct MissionArtifactIo {
    pub path: PathBuf,
    pub source: io::Error,
}

#[derive(Clone, Debug)]
pub struct MissionArtifactStore {
    root: PathBuf,
    file_name: &'static str,
}

impl MissionArtifactStore {
    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
        file_name: &'static str,
    ) -> Result<Self, MissionArtifactIo> {
        let root = magi_core::paths::missions_root(magi_home, workspace_root);
        std::fs::create_dir_all(&root).map_err(|source| MissionArtifactIo {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root, file_name })
    }

    pub fn path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join(self.file_name)
    }

    pub fn load_text(&self, mission_id: &MissionId) -> Result<Option<String>, MissionArtifactIo> {
        let path = self.path(mission_id);
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(Some(raw)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(MissionArtifactIo { path, source }),
        }
    }

    pub fn save_text(
        &self,
        mission_id: &MissionId,
        contents: impl AsRef<str>,
    ) -> Result<(), MissionArtifactIo> {
        let path = self.path(mission_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| MissionArtifactIo {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        magi_core::fs_atomic::write_atomic(&path, contents.as_ref())
            .map_err(|source| MissionArtifactIo { path, source })
    }
}

pub struct MissionArtifactRegistry<S> {
    inner: RwLock<HashMap<String, Arc<S>>>,
    magi_home: PathBuf,
}

impl<S> MissionArtifactRegistry<S> {
    pub fn with_magi_home(magi_home: impl Into<PathBuf>) -> Self {
        let magi_home = magi_home.into();
        let _ = std::fs::create_dir_all(&magi_home);
        Self {
            inner: RwLock::new(HashMap::new()),
            magi_home,
        }
    }

    pub fn get_or_open<E>(
        &self,
        workspace_root: &WorkspaceRootPath,
        open: impl FnOnce(&Path, &WorkspaceRootPath) -> Result<S, E>,
    ) -> Result<Arc<S>, E> {
        let key = workspace_root.as_str().to_string();
        if let Some(store) = self
            .inner
            .read()
            .expect("mission artifact registry read lock poisoned")
            .get(&key)
        {
            return Ok(store.clone());
        }
        let store = open(&self.magi_home, workspace_root)?;
        let store = Arc::new(store);
        self.inner
            .write()
            .expect("mission artifact registry write lock poisoned")
            .insert(key, store.clone());
        Ok(store)
    }
}

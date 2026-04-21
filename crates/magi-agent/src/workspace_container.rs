use crate::session_context::SessionExecutionContextStats;
use crate::session_registry::SessionRuntimeRegistry;

pub struct WorkspaceDescriptor {
    pub workspace_id: String,
    pub name: String,
    pub root_path: String,
}

pub struct WorkspaceRuntimeContainerStats {
    pub workspace_id: String,
    pub workspace_name: String,
    pub workspace_root: String,
    pub session_count: usize,
    pub sessions: Vec<SessionExecutionContextStats>,
}

pub struct WorkspaceRuntimeContainer {
    workspace: WorkspaceDescriptor,
    session_registry: SessionRuntimeRegistry,
}

impl WorkspaceRuntimeContainer {
    pub fn new(workspace: WorkspaceDescriptor) -> Self {
        Self {
            workspace,
            session_registry: SessionRuntimeRegistry::default(),
        }
    }

    pub fn with_session_registry(workspace: WorkspaceDescriptor, registry: SessionRuntimeRegistry) -> Self {
        Self {
            workspace,
            session_registry: registry,
        }
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace.workspace_id
    }

    pub fn workspace_name(&self) -> &str {
        &self.workspace.name
    }

    pub fn workspace_root(&self) -> &str {
        &self.workspace.root_path
    }

    pub fn session_registry(&self) -> &SessionRuntimeRegistry {
        &self.session_registry
    }

    pub fn session_registry_mut(&mut self) -> &mut SessionRuntimeRegistry {
        &mut self.session_registry
    }

    pub fn prune_idle_sessions(&mut self) -> Vec<String> {
        self.session_registry.prune_idle()
    }

    pub fn clear(&mut self) {
        self.session_registry.clear();
    }

    pub fn stats(&self) -> WorkspaceRuntimeContainerStats {
        WorkspaceRuntimeContainerStats {
            workspace_id: self.workspace.workspace_id.clone(),
            workspace_name: self.workspace.name.clone(),
            workspace_root: self.workspace.root_path.clone(),
            session_count: self.session_registry.size(),
            sessions: self.session_registry.stats(),
        }
    }
}

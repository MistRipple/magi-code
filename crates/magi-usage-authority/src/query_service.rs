use crate::authority::UsageAuthority;
use crate::types::{SessionUsageSnapshot, WorkspaceUsageSnapshot};

pub struct UsageQueryService<'a> {
    authority: &'a mut UsageAuthority,
}

impl<'a> UsageQueryService<'a> {
    pub fn new(authority: &'a mut UsageAuthority) -> Self {
        Self { authority }
    }

    pub fn session_snapshot(
        &mut self,
        workspace_id: &str,
        session_id: &str,
    ) -> SessionUsageSnapshot {
        self.authority
            .get_session_snapshot(workspace_id, session_id)
    }

    pub fn workspace_snapshot(&mut self, workspace_id: &str) -> WorkspaceUsageSnapshot {
        self.authority.get_workspace_snapshot(workspace_id)
    }
}

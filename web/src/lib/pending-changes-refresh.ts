import { applyPendingChangesProjection } from '../stores/messages.svelte';
import { synchronizeChangeDiffTabs } from '../stores/right-pane.svelte';
import { getAgentPendingChanges } from '../web/agent-api';
import type { Edit } from '../types/message';

export interface PendingChangesRefreshScope {
  scope: 'workspace';
  sessionId?: string;
  workspaceId: string;
  workspacePath: string;
  forceRefresh?: boolean;
}

export async function refreshPendingChangesProjection(
  scope: PendingChangesRefreshScope,
): Promise<boolean> {
  const payload = await getAgentPendingChanges(scope);
  const applied = applyPendingChangesProjection(payload);
  if (applied) {
    synchronizeChangeDiffTabs(
      payload.workspaceId,
      payload.sessionId,
      payload.pendingChanges as Edit[],
    );
  }
  return applied;
}

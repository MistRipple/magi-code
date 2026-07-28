import { applyPendingChangesProjection } from '../stores/messages.svelte';
import { getAgentPendingChanges } from '../web/agent-api';

export interface PendingChangesRefreshScope {
  sessionId?: string;
  workspaceId?: string;
  workspacePath?: string;
}

export async function refreshPendingChangesProjection(
  scope: PendingChangesRefreshScope,
): Promise<boolean> {
  const payload = await getAgentPendingChanges(scope);
  return applyPendingChangesProjection(payload);
}

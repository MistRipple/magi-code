<script lang="ts">
  /**
   * 外部调用的全局待确认托盘：外部调用不属于用户当前打开的会话，所以不能复用会话内的授权托盘。
   * 这里是“你不在那个外部会话里时”的提醒入口；在外部会话里，同一个确认出现在输入区位置。
   * 事实与操作都在 external-approvals 存储里，托盘不保存任何状态。
   */
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { externalApprovals, trackExternalApprovals, selectExternalDirectory } from '../stores/external-approvals.svelte';
  import { externalSessionById, externalSessions, openExternalSession } from '../stores/external-sessions.svelte';
  import { closeSettings } from '../stores/shell-ui.svelte';
  import { composerWorkspaceState } from '../stores/composer-workspace.svelte';
  import { messagesState } from '../stores/messages.svelte';
  import SelectField from './SelectField.svelte';
  import WebFolderPicker from '../web/WebFolderPicker.svelte';
  import { registerAgentWorkspace, type AgentWorkspaceSummary } from '../web/agent-api';
  import { addToast } from '../stores/messages.svelte';
  import ExternalApprovalActions from './ExternalApprovalActions.svelte';

  onMount(() => trackExternalApprovals());

  /** 正在这个外部会话里：待确认已经出现在输入区位置，托盘不重复打扰。 */
  function isViewedInSession(sessionId: string): boolean {
    return externalSessionById(messagesState.currentSessionId)?.sessionId === sessionId;
  }

  const visible = $derived(externalApprovals.approvals.filter((approval) => !isViewedInSession(approval.sessionId)));
  let directoryChoices = $state<Record<string, string>>({});
  let choosingFor = $state('');
  let registering = $state(false);
  let registeredWorkspaces = $state<AgentWorkspaceSummary[] | null>(null);

  async function addDirectory(selection: { pathRef: string }): Promise<void> {
    const sessionId = choosingFor;
    registering = true;
    try {
      const registration = await registerAgentWorkspace(selection.pathRef);
      registeredWorkspaces = registration.workspaces;
      const workspace = registration.workspaces.find(item => item.workspaceId === registration.workspaceId);
      const folder = workspace?.sourceFolders?.find(item => item.sourceFolderId === workspace.primarySourceFolderId);
      if (workspace && folder) directoryChoices[sessionId] = JSON.stringify([workspace.workspaceId, folder.sourceFolderId]);
      choosingFor = '';
    } catch (error) {
      addToast('error', error instanceof Error ? error.message : String(error));
    } finally { registering = false; }
  }

  const directories = $derived((registeredWorkspaces ?? composerWorkspaceState.workspaces).flatMap(workspace => (workspace.sourceFolders ?? []).map(folder => ({
    value: JSON.stringify([workspace.workspaceId, folder.sourceFolderId]),
    label: `${workspace.name} · ${folder.path}`,
    workspaceId: workspace.workspaceId, sourceFolderId: folder.sourceFolderId,
  }))));

  function approveDirectory(sessionId: string): void {
    const directory = directories.find(item => item.value === directoryChoices[sessionId]);
    if (directory) void selectExternalDirectory(sessionId, { workspaceId: directory.workspaceId, sourceFolderId: directory.sourceFolderId });
  }
</script>

{#if visible.length > 0 || externalApprovals.directories.length > 0}
  <div class="tray" data-external-approval-tray="1" role="alertdialog" aria-label={i18n.t('mcpServer.tray.title')}>
    <div class="tray-title">{i18n.t('mcpServer.tray.title')}</div>
    {#each externalApprovals.directories as request (request.sessionId)}
      <div class="item" data-mcp-directory-request={request.sessionId}>
        <div class="summary">{i18n.t('mcpServer.conversation.chooseTitle')}</div>
        <div class="meta">{request.title || i18n.t('externalSession.title')} · {request.clientName}</div>
        <p class="meta">{i18n.t('mcpServer.conversation.chooseHint')}</p>
        <SelectField bind:value={directoryChoices[request.sessionId]} label={i18n.t('mcpServer.conversation.directory')}
          options={[{value: '', label: i18n.t('mcpServer.conversation.placeholder')}, ...directories]} />
        <button type="button" class="btn btn--sm btn--secondary" disabled={externalApprovals.busy} onclick={() => { choosingFor = request.sessionId; }}>{i18n.t('mcpServer.conversation.browse')}</button>
        <div class="directory-actions">
          <button type="button" class="btn btn--sm btn--primary" disabled={externalApprovals.busy || !directories.some(item => item.value === directoryChoices[request.sessionId])} onclick={() => approveDirectory(request.sessionId)}>{i18n.t('mcpServer.conversation.allow')}</button>
          <button type="button" class="btn btn--sm btn--secondary" disabled={externalApprovals.busy} onclick={() => void selectExternalDirectory(request.sessionId, null)}>{i18n.t('mcpServer.conversation.deny')}</button>
        </div>
      </div>
    {/each}
    {#each visible as approval (approval.approvalId)}
      {@const session = externalSessions.sessions.find((entry) => entry.sessionId === approval.sessionId)}
      {@const workspace = composerWorkspaceState.workspaces.find((entry) => entry.workspaceId === approval.workspaceId)}
      <div class="item" data-external-approval={approval.approvalId}>
        <div class="summary">{approval.summary}</div>
        <div class="meta">{i18n.t('mcpServer.approvals.from', { client: approval.clientName, prefix: approval.tokenPrefix })}</div>
        <div class="meta ui-meta"><span>{i18n.t('mcpServer.token.boundWorkspace', { workspace: workspace?.name ?? approval.workspaceId })}</span></div>
        {#if session}
          <button type="button" class="btn btn--sm btn--secondary" data-mcp-approval-view-session={approval.approvalId} onclick={() => { closeSettings(); openExternalSession(session); }}>
            {i18n.t('mcpServer.audit.viewSession')}
          </button>
        {/if}
        <ExternalApprovalActions {approval} />
      </div>
    {/each}
  </div>
{/if}

{#if choosingFor}
  <WebFolderPicker title={i18n.t('mcpServer.conversation.directory')} disabled={registering} onCancel={() => { choosingFor = ''; }} onSelect={(selection) => void addDirectory(selection)} />
{/if}

<style>
  .directory-actions { display: flex; flex-wrap: wrap; gap: 8px; }
  .tray {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 1200;
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(clamp(320px, 36vw, 520px), calc(100vw - 32px));
    max-height: calc(100dvh - 32px);
    overflow-y: auto;
    padding: 12px 14px;
    border: var(--magi-hairline) solid var(--border-subtle, rgba(127, 127, 127, 0.28));
    border-radius: 12px;
    background: var(--surface-elevated, var(--surface, #fff));
    box-shadow: var(--shadow-lg);
  }

  .tray-title {
    font-size: 12px;
    font-weight: var(--font-semibold);
  }

  .item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
  }

  .summary {
    font-size: 12px;
    font-weight: var(--font-semibold);
    overflow-wrap: anywhere;
  }

  .meta {
    overflow-wrap: anywhere;
    font-size: var(--text-xs);
    opacity: 0.66;
  }
</style>

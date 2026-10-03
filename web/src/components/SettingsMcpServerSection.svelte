<script lang="ts">
  /**
   * 「设置 → 能力 → Magi MCP 服务」分区。
   *
   * 只展示 daemon 的事实：服务是否运行、令牌列表、待确认调用与审计记录都来自
   * `/api/mcp-server/*`。令牌原文由 daemon 在本机保存，需要时通过“查看配置”重新取回；
   * 这里不落任何浏览器存储，收起面板或关闭提示卡后即丢弃。
   */
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { composerWorkspaceState } from '../stores/composer-workspace.svelte';
  import { addToast } from '../stores/messages.svelte';
  import {
    createMcpServerToken,
    getMcpServerConfigSnippets,
    getMcpServerStatus,
    getMcpServerTokenSecret,
    listMcpServerApprovals,
    listMcpServerAudit,
    resolveMcpServerApproval,
    revokeAllMcpServerTokens,
    revokeMcpServerToken,
    rotateMcpServerToken,
    setMcpServerEnabled,
    setMcpServerNetwork,
    updateMcpServerToken,
    type McpServerApproval,
    type McpServerAuditEntry,
    type McpServerConfigSnippets,
    type McpServerProfile,
    type McpServerStatus,
    type McpServerToken,
  } from '../web/agent-api';
  import { MCP_APPROVALS_CHANGED_EVENT } from '../lib/mcp-server-events';

  const PROFILES: readonly McpServerProfile[] = ['read_only', 'edit', 'edit_trusted', 'exec'];
  const HIGH_RISK_PROFILES: readonly McpServerProfile[] = ['edit_trusted', 'exec'];
  const APPROVAL_POLL_MS = 3000;

  let status = $state<McpServerStatus | null>(null);
  let snippets = $state<McpServerConfigSnippets | null>(null);
  let approvals = $state<McpServerApproval[]>([]);
  let audit = $state<McpServerAuditEntry[]>([]);
  let loadError = $state('');
  let busy = $state(false);
  let now = $state(Date.now());

  let clientName = $state('');
  let workspaceId = $state('');
  let profile = $state<McpServerProfile>('edit');
  let ttlDays = $state(90);
  let confirmHighRisk = $state(false);
  let allowNetwork = $state(false);
  /** 刚创建的令牌原文。只驻内存，用户点“我已保存”后丢弃。 */
  let freshSecret = $state('');
  let copiedKey = $state('');

  /** 展开“查看配置”的令牌：原文与带原文的配置都来自 daemon，收起即丢弃。 */
  let viewingTokenId = $state('');
  let viewingSecret = $state<string | null>(null);
  let viewingSnippets = $state<McpServerConfigSnippets | null>(null);

  /** 正在编辑的令牌。工作区创建后不可改。 */
  let editingTokenId = $state('');
  let editName = $state('');
  let editProfile = $state<McpServerProfile>('edit');
  let editNetwork = $state(false);
  let editTtl = $state('');
  let editConfirmHighRisk = $state(false);

  const workspaces = $derived(composerWorkspaceState.workspaces);
  const needsConfirm = $derived(HIGH_RISK_PROFILES.includes(profile));
  const hasNetworkToken = $derived(
    (status?.tokens ?? []).some((token) => token.network && token.active),
  );
  const networkTunnelStatus = $derived(status?.network.status ?? 'stopped');
  const editingToken = $derived(
    (status?.tokens ?? []).find((token) => token.tokenId === editingTokenId) ?? null,
  );
  const editNeedsConfirm = $derived(
    HIGH_RISK_PROFILES.includes(editProfile) && editProfile !== editingToken?.profile,
  );
  const editHighRisk = $derived(HIGH_RISK_PROFILES.includes(editProfile));
  const canSaveEdit = $derived(
    !busy
      && editName.trim() !== ''
      && (editTtl.trim() === '' || /^\d+$/.test(editTtl.trim()))
      && (!editNeedsConfirm || editConfirmHighRisk),
  );
  const canCreate = $derived(
    !busy
      && clientName.trim() !== ''
      && workspaceId !== ''
      && (!needsConfirm || confirmHighRisk),
  );

  function messageOf(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  async function refresh(): Promise<void> {
    try {
      status = await getMcpServerStatus();
      loadError = '';
      const [nextApprovals, nextAudit, nextSnippets] = await Promise.all([
        listMcpServerApprovals(),
        listMcpServerAudit({ limit: 30 }),
        status.running ? getMcpServerConfigSnippets() : Promise.resolve(null),
      ]);
      approvals = nextApprovals;
      audit = nextAudit;
      snippets = nextSnippets;
    } catch (error) {
      loadError = i18n.t('mcpServer.error.load', { message: messageOf(error) });
    }
  }

  async function refreshApprovals(): Promise<void> {
    try {
      approvals = await listMcpServerApprovals();
    } catch {
      /* 下一次轮询再试；错误由完整刷新展示 */
    }
  }

  async function act(action: () => Promise<void>): Promise<void> {
    busy = true;
    try {
      await action();
    } catch (error) {
      addToast('error', i18n.t('mcpServer.error.action', { message: messageOf(error) }));
    } finally {
      busy = false;
    }
  }

  function toggleEnabled(enabled: boolean): Promise<void> {
    return act(async () => {
      status = await setMcpServerEnabled(enabled);
      await refresh();
    });
  }

  function createToken(): Promise<void> {
    return act(async () => {
      const created = await createMcpServerToken({
        clientName: clientName.trim(),
        workspaceId,
        profile,
        ttlDays,
        confirmHighRisk: needsConfirm ? confirmHighRisk : undefined,
        allowNetwork: allowNetwork && !needsConfirm ? true : undefined,
      });
      freshSecret = created.secret;
      clientName = '';
      confirmHighRisk = false;
      allowNetwork = false;
      await refresh();
    });
  }

  function toggleNetwork(enabled: boolean): Promise<void> {
    if (enabled && !window.confirm(i18n.t('mcpServer.network.confirm'))) {
      void refresh();
      return Promise.resolve();
    }
    return act(async () => {
      status = await setMcpServerNetwork(enabled, enabled);
      await refresh();
    });
  }

  function revoke(token: McpServerToken): Promise<void> {
    return act(async () => {
      status = await revokeMcpServerToken(token.tokenId);
      if (viewingTokenId === token.tokenId) toggleViewOff();
      if (editingTokenId === token.tokenId) editingTokenId = '';
      await refresh();
    });
  }

  function toggleViewOff(): void {
    viewingTokenId = '';
    viewingSecret = null;
    viewingSnippets = null;
  }

  async function loadView(token: McpServerToken): Promise<void> {
    viewingSecret = null;
    viewingSnippets = null;
    if (token.hasSecret) {
      viewingSecret = await getMcpServerTokenSecret(token.tokenId);
    }
    if (status?.running) {
      viewingSnippets = await getMcpServerConfigSnippets(token.tokenId);
    }
  }

  function toggleView(token: McpServerToken): Promise<void> {
    if (viewingTokenId === token.tokenId) {
      viewingTokenId = '';
      viewingSecret = null;
      viewingSnippets = null;
      return Promise.resolve();
    }
    viewingTokenId = token.tokenId;
    return act(() => loadView(token));
  }

  function startEdit(token: McpServerToken): void {
    editingTokenId = token.tokenId;
    editName = token.clientName;
    editProfile = token.profile;
    editNetwork = token.network;
    editTtl = '';
    editConfirmHighRisk = false;
  }

  function cancelEdit(): void {
    editingTokenId = '';
  }

  function saveEdit(): Promise<void> {
    const token = editingToken;
    if (!token) return Promise.resolve();
    return act(async () => {
      const ttl = editTtl.trim();
      // 高风险权限档不能同时开放公网：升到高风险档时一并关掉。
      const network = editHighRisk ? false : editNetwork;
      status = await updateMcpServerToken(token.tokenId, {
        clientName: editName.trim() !== token.clientName ? editName.trim() : undefined,
        profile: editProfile !== token.profile ? editProfile : undefined,
        allowNetwork: network !== token.network ? network : undefined,
        ttlDays: ttl === '' ? undefined : Number(ttl),
        confirmHighRisk: editNeedsConfirm ? editConfirmHighRisk : undefined,
      });
      editingTokenId = '';
      await refresh();
      const updated = status?.tokens.find((entry) => entry.tokenId === token.tokenId);
      if (updated && viewingTokenId === token.tokenId) await loadView(updated);
    });
  }

  function rotate(token: McpServerToken): Promise<void> {
    if (!window.confirm(i18n.t('mcpServer.token.rotateConfirm', { client: token.clientName }))) {
      return Promise.resolve();
    }
    return act(async () => {
      const rotated = await rotateMcpServerToken(token.tokenId);
      freshSecret = rotated.secret;
      await refresh();
      const updated = status?.tokens.find((entry) => entry.tokenId === token.tokenId);
      if (updated && viewingTokenId === token.tokenId) await loadView(updated);
    });
  }

  function revokeAll(): Promise<void> {
    if (!window.confirm(i18n.t('mcpServer.token.revokeAllConfirm'))) return Promise.resolve();
    return act(async () => {
      status = await revokeAllMcpServerTokens();
      toggleViewOff();
      editingTokenId = '';
      await refresh();
    });
  }

  function resolveApproval(approval: McpServerApproval, decision: 'allow_once' | 'deny'): Promise<void> {
    return act(async () => {
      approvals = await resolveMcpServerApproval(approval.approvalId, decision);
      audit = await listMcpServerAudit({ limit: 30 });
    });
  }

  async function copy(key: string, text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      copiedKey = key;
      window.setTimeout(() => {
        if (copiedKey === key) copiedKey = '';
      }, 1500);
    } catch {
      /* 剪贴板不可用时用户仍可手动选择文本 */
    }
  }

  function snippetEntries(
    source: McpServerConfigSnippets,
  ): { key: string; label: string; text: string }[] {
    const entries: { key: string; label: string; text: string }[] = [];
    const json = (value: Record<string, unknown>) => JSON.stringify(value, null, 2);
    if (source.httpJson) entries.push({ key: 'http', label: i18n.t('mcpServer.snippets.http'), text: json(source.httpJson) });
    if (source.stdioJson) entries.push({ key: 'stdio', label: i18n.t('mcpServer.snippets.stdio'), text: json(source.stdioJson) });
    if (source.claudeCli) entries.push({ key: 'cli', label: i18n.t('mcpServer.snippets.claudeCli'), text: source.claudeCli });
    if (source.remoteJson) entries.push({ key: 'remote', label: i18n.t('mcpServer.snippets.remote'), text: json(source.remoteJson) });
    return entries;
  }

  function workspaceName(id: string): string {
    return workspaces.find((workspace) => workspace.workspaceId === id)?.name ?? id;
  }

  function formatTime(ms: number): string {
    return new Date(ms).toLocaleString();
  }

  function tokenStateLabel(token: McpServerToken): string {
    if (token.revokedAtMs !== null) return i18n.t('mcpServer.token.revoked');
    if (!token.active) return i18n.t('mcpServer.token.expired');
    return '';
  }

  function secondsLeft(approval: McpServerApproval): number {
    return Math.max(0, Math.ceil((approval.expiresAtMs - now) / 1000));
  }

  $effect(() => {
    if (needsConfirm) allowNetwork = false;
  });

  $effect(() => {
    if (!workspaceId && workspaces.length > 0) {
      workspaceId = workspaces.find((workspace) => workspace.isActive)?.workspaceId
        ?? workspaces[0].workspaceId;
    }
  });

  onMount(() => {
    void refresh();
    const timer = window.setInterval(() => {
      now = Date.now();
      void refreshApprovals();
      // 隧道建立需要几秒：网络模式开启但还没就绪时，跟随刷新状态以拿到公网地址。
      if (status?.network.enabled && status.network.status !== 'running') void refresh();
    }, APPROVAL_POLL_MS);
    const clock = window.setInterval(() => { now = Date.now(); }, 1000);
    const onChanged = () => { void refreshApprovals(); };
    window.addEventListener(MCP_APPROVALS_CHANGED_EVENT, onChanged);
    return () => {
      window.clearInterval(timer);
      window.clearInterval(clock);
      window.removeEventListener(MCP_APPROVALS_CHANGED_EVENT, onChanged);
    };
  });
</script>

<section class="mcp-server-section" data-mcp-server="1">
  <div class="section-heading">
    <div>
      <h3>{i18n.t('mcpServer.title')}</h3>
      <p>{i18n.t('mcpServer.desc')}</p>
    </div>
  </div>

  {#if loadError}
    <p class="status-message error" role="alert">{loadError}</p>
  {/if}

  <div class="card" data-mcp-server-status="1">
    <label class="switch-line">
      <input
        type="checkbox"
        data-mcp-server-enabled="1"
        checked={status?.enabled ?? false}
        disabled={busy || status === null}
        onchange={(event) => void toggleEnabled((event.currentTarget as HTMLInputElement).checked)}
      />
      <span>{i18n.t('mcpServer.enable')}</span>
      <span class="badge" class:badge--off={!status?.running}>
        {status?.running ? i18n.t('mcpServer.status.running') : i18n.t('mcpServer.status.stopped')}
      </span>
    </label>
    <p class="meta">{i18n.t('mcpServer.enableHint')}</p>
    {#if status?.running}
      {#if status.url}<p class="meta"><code>{i18n.t('mcpServer.status.url', { url: status.url })}</code></p>{/if}
      {#if status.stdioEndpoint}<p class="meta"><code>{i18n.t('mcpServer.status.stdio', { endpoint: status.stdioEndpoint })}</code></p>{/if}
      {#if status.port}<p class="meta">{i18n.t('mcpServer.status.port', { port: status.port })}</p>{/if}
    {/if}
  </div>

  <div class="card" data-mcp-server-network="1">
    <h4>{i18n.t('mcpServer.network.section')}</h4>
    <p class="meta">{i18n.t('mcpServer.network.desc')}</p>
    <p class="meta">{i18n.t('mcpServer.network.addressChanges')}</p>
    <label class="switch-line">
      <input
        type="checkbox"
        data-mcp-network-enabled="1"
        checked={status?.network.enabled ?? false}
        disabled={busy || status === null || (!status.network.enabled && !hasNetworkToken)}
        onchange={(event) => void toggleNetwork((event.currentTarget as HTMLInputElement).checked)}
      />
      <span>{i18n.t('mcpServer.network.enable')}</span>
      <span class="badge" class:badge--off={networkTunnelStatus !== 'running'}>
        {i18n.t(`mcpServer.network.status.${networkTunnelStatus}`)}
      </span>
    </label>
    {#if status && !status.network.enabled && !hasNetworkToken}
      <p class="meta">{i18n.t('mcpServer.network.needToken')}</p>
    {/if}
    {#if status?.network.mcpUrl}
      <div class="snippet-head">
        <span class="meta">{i18n.t('mcpServer.network.url')}</span>
        <button type="button" onclick={() => void copy('network-url', status?.network.mcpUrl ?? '')}>
          {copiedKey === 'network-url' ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
        </button>
      </div>
      <code class="secret" data-mcp-network-url="1">{status.network.mcpUrl}</code>
    {/if}
    {#if status?.network.error}
      <p class="status-message error" role="alert">{status.network.error}</p>
    {/if}
    <p class="meta">{i18n.t('mcpServer.network.autoStop')}</p>
  </div>

  <div class="card" data-mcp-server-approvals="1">
    <h4>{i18n.t('mcpServer.approvals.section')}</h4>
    {#if approvals.length === 0}
      <p class="meta">{i18n.t('mcpServer.approvals.empty')}</p>
    {:else}
      <ul class="rows">
        {#each approvals as approval (approval.approvalId)}
          <li data-mcp-approval={approval.approvalId}>
            <div class="row-main">
              <span class="row-title">{approval.summary}</span>
              <span class="meta">
                {i18n.t('mcpServer.approvals.from', { client: approval.clientName, prefix: approval.tokenPrefix })}
                · {i18n.t('mcpServer.approvals.expiresIn', { seconds: secondsLeft(approval) })}
              </span>
            </div>
            <div class="action-row">
              <button type="button" class="primary-action" disabled={busy} onclick={() => void resolveApproval(approval, 'allow_once')}>
                {i18n.t('mcpServer.approvals.allow')}
              </button>
              <button type="button" disabled={busy} onclick={() => void resolveApproval(approval, 'deny')}>
                {i18n.t('mcpServer.approvals.deny')}
              </button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <div class="card" data-mcp-server-tokens="1">
    <h4>{i18n.t('mcpServer.token.section')}</h4>

    {#if freshSecret}
      <div class="secret-card" data-mcp-fresh-secret="1" role="status">
        <p class="meta">{i18n.t('mcpServer.token.created')}</p>
        <code class="secret">{freshSecret}</code>
        <div class="action-row">
          <button type="button" class="primary-action" onclick={() => void copy('secret', freshSecret)}>
            {copiedKey === 'secret' ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
          </button>
          <button type="button" onclick={() => { freshSecret = ''; }}>{i18n.t('mcpServer.token.dismiss')}</button>
        </div>
      </div>
    {/if}

    {#if status && status.tokens.length === 0}
      <p class="meta">{i18n.t('mcpServer.token.empty')}</p>
    {:else if status}
      <ul class="rows">
        {#each status.tokens as token (token.tokenId)}
          <li data-mcp-token={token.tokenId} class:inactive={!token.active}>
            <div class="row-main">
              <span class="row-title">
                {token.clientName}
                <code>{token.prefix}…</code>
                <span class="badge">{i18n.t(`mcpServer.token.profileShort.${token.profile}`)}</span>
                {#if token.network}<span class="badge" data-mcp-token-network="1">{i18n.t('mcpServer.token.networkBadge')}</span>{/if}
                {#if tokenStateLabel(token)}<span class="badge badge--off">{tokenStateLabel(token)}</span>{/if}
              </span>
              <span class="meta">
                {workspaceName(token.workspaceId)}
                · {token.expiresAtMs === null ? i18n.t('mcpServer.token.neverExpires') : i18n.t('mcpServer.token.expiresAt', { time: formatTime(token.expiresAtMs) })}
                · {token.lastUsedAtMs === null ? i18n.t('mcpServer.token.neverUsed') : i18n.t('mcpServer.token.lastUsed', { time: formatTime(token.lastUsedAtMs) })}
              </span>
            </div>
            {#if token.active}
              <div class="action-row">
                <button type="button" data-mcp-token-view={token.tokenId} disabled={busy} onclick={() => void toggleView(token)}>
                  {viewingTokenId === token.tokenId ? i18n.t('mcpServer.token.hide') : i18n.t('mcpServer.token.view')}
                </button>
                <button type="button" data-mcp-token-edit={token.tokenId} disabled={busy || token.attribution !== 'external'} onclick={() => startEdit(token)}>
                  {i18n.t('mcpServer.token.edit')}
                </button>
                <button type="button" disabled={busy || token.attribution !== 'external'} onclick={() => void rotate(token)}>
                  {i18n.t('mcpServer.token.rotate')}
                </button>
                <button type="button" class="danger" disabled={busy} onclick={() => void revoke(token)}>
                  {i18n.t('mcpServer.token.revoke')}
                </button>
              </div>
            {/if}
          </li>
          {#if viewingTokenId === token.tokenId}
            <li class="detail" data-mcp-token-detail={token.tokenId}>
              {#if viewingSecret}
                <div class="snippet-head">
                  <span class="row-title">{i18n.t('mcpServer.token.secret')}</span>
                  <button type="button" onclick={() => void copy(`secret-${token.tokenId}`, viewingSecret ?? '')}>
                    {copiedKey === `secret-${token.tokenId}` ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
                  </button>
                </div>
                <code class="secret" data-mcp-token-secret="1">{viewingSecret}</code>
              {:else}
                <p class="meta" data-mcp-token-legacy="1">{i18n.t('mcpServer.token.noSecret')}</p>
              {/if}
              {#if viewingSnippets}
                <p class="meta">{i18n.t('mcpServer.snippets.perClientHint', { name: viewingSnippets.serverName })}</p>
                {#each snippetEntries(viewingSnippets) as entry (entry.key)}
                  <div class="snippet">
                    <div class="snippet-head">
                      <span class="row-title">{entry.label}</span>
                      <button type="button" onclick={() => void copy(`${entry.key}-${token.tokenId}`, entry.text)}>
                        {copiedKey === `${entry.key}-${token.tokenId}` ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
                      </button>
                    </div>
                    <pre>{entry.text}</pre>
                  </div>
                {/each}
              {:else if !status?.running}
                <p class="meta">{i18n.t('mcpServer.snippets.needStart')}</p>
              {/if}
            </li>
          {/if}
          {#if editingTokenId === token.tokenId}
            <li class="detail" data-mcp-token-edit-form={token.tokenId}>
              <form class="create-form edit-form" onsubmit={(event) => { event.preventDefault(); void saveEdit(); }}>
                <label class="field">
                  <span>{i18n.t('mcpServer.token.clientName')}</span>
                  <input bind:value={editName} maxlength="60" data-mcp-edit-name="1" />
                </label>
                <p class="meta">{i18n.t('mcpServer.token.editWorkspaceFixed', { workspace: workspaceName(token.workspaceId) })}</p>
                <label class="field">
                  <span>{i18n.t('mcpServer.token.profile')}</span>
                  <select bind:value={editProfile} data-mcp-edit-profile="1">
                    {#each PROFILES as option (option)}
                      <option value={option}>{i18n.t(`mcpServer.token.profile.${option}`)}</option>
                    {/each}
                  </select>
                </label>
                <label class="field">
                  <span>{i18n.t('mcpServer.token.editTtl')}</span>
                  <input bind:value={editTtl} inputmode="numeric" placeholder={i18n.t('mcpServer.token.editTtlKeep')} data-mcp-edit-ttl="1" />
                </label>
                {#if !editHighRisk}
                  <label class="switch-line">
                    <input type="checkbox" bind:checked={editNetwork} />
                    <span>{i18n.t('mcpServer.token.allowNetwork')}</span>
                  </label>
                {:else if token.network}
                  <p class="meta">{i18n.t('mcpServer.token.editNetworkDropped')}</p>
                {/if}
                {#if editNeedsConfirm}
                  <label class="switch-line risk">
                    <input type="checkbox" bind:checked={editConfirmHighRisk} />
                    <span>{i18n.t('mcpServer.token.confirmHighRisk')}</span>
                  </label>
                {/if}
                <p class="meta">{i18n.t('mcpServer.token.editEffect')}</p>
                <div class="action-row">
                  <button type="submit" class="primary-action" disabled={!canSaveEdit}>{i18n.t('mcpServer.token.save')}</button>
                  <button type="button" onclick={cancelEdit}>{i18n.t('mcpServer.token.cancel')}</button>
                </div>
              </form>
            </li>
          {/if}
        {/each}
      </ul>
      {#if status.tokens.some((token) => token.active)}
        <div class="action-row">
          <button type="button" class="danger" disabled={busy} onclick={() => void revokeAll()}>
            {i18n.t('mcpServer.token.revokeAll')}
          </button>
        </div>
      {/if}
    {/if}

    <form class="create-form" onsubmit={(event) => { event.preventDefault(); void createToken(); }}>
      <label class="field">
        <span>{i18n.t('mcpServer.token.clientName')}</span>
        <input
          bind:value={clientName}
          data-mcp-client-name="1"
          placeholder={i18n.t('mcpServer.token.clientNamePlaceholder')}
          maxlength="60"
        />
      </label>
      <label class="field">
        <span>{i18n.t('mcpServer.token.workspace')}</span>
        <select bind:value={workspaceId} data-mcp-workspace="1" disabled={workspaces.length === 0}>
          {#if workspaces.length === 0}
            <option value="">{i18n.t('mcpServer.token.workspaceEmpty')}</option>
          {/if}
          {#each workspaces as workspace (workspace.workspaceId)}
            <option value={workspace.workspaceId}>{workspace.name}</option>
          {/each}
        </select>
      </label>
      <label class="field">
        <span>{i18n.t('mcpServer.token.profile')}</span>
        <select bind:value={profile} data-mcp-profile="1">
          {#each PROFILES as option (option)}
            <option value={option}>{i18n.t(`mcpServer.token.profile.${option}`)}</option>
          {/each}
        </select>
      </label>
      <label class="field">
        <span>{i18n.t('mcpServer.token.ttl')}</span>
        <input type="number" min="0" max="3650" step="1" bind:value={ttlDays} data-mcp-ttl="1" />
      </label>
      {#if needsConfirm}
        <label class="switch-line risk" data-mcp-confirm-risk="1">
          <input type="checkbox" bind:checked={confirmHighRisk} />
          <span>{i18n.t('mcpServer.token.confirmHighRisk')}</span>
        </label>
      {/if}
      {#if !needsConfirm}
        <label class="switch-line" data-mcp-allow-network="1">
          <input type="checkbox" bind:checked={allowNetwork} />
          <span>{i18n.t('mcpServer.token.allowNetwork')}</span>
        </label>
      {/if}
      <div class="action-row">
        <button type="submit" class="primary-action" data-mcp-create="1" disabled={!canCreate}>
          {i18n.t('mcpServer.token.create')}
        </button>
      </div>
    </form>
  </div>

  <div class="card" data-mcp-server-snippets="1">
    <h4>{i18n.t('mcpServer.snippets.section')}</h4>
    {#if !snippets}
      <p class="meta">{i18n.t('mcpServer.snippets.needStart')}</p>
    {:else}
      <p class="meta">{i18n.t('mcpServer.snippets.hint')}</p>
      {#each snippetEntries(snippets) as entry (entry.key)}
        <div class="snippet">
          <div class="snippet-head">
            <span class="row-title">{entry.label}</span>
            <button type="button" onclick={() => void copy(entry.key, entry.text)}>
              {copiedKey === entry.key ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
            </button>
          </div>
          <pre>{entry.text}</pre>
        </div>
      {/each}
    {/if}
  </div>

  <div class="card" data-mcp-server-audit="1">
    <h4>{i18n.t('mcpServer.audit.section')}</h4>
    {#if audit.length === 0}
      <p class="meta">{i18n.t('mcpServer.audit.empty')}</p>
    {:else}
      <ul class="rows">
        {#each audit as entry, index (`${entry.atMs}-${entry.tokenId}-${index}`)}
          <li>
            <div class="row-main">
              <span class="row-title">
                <code>{entry.tool}</code>
                <span class="badge" class:badge--off={entry.outcome !== 'succeeded'}>
                  {i18n.t(`mcpServer.audit.outcome.${entry.outcome}`)}
                </span>
              </span>
              <span class="meta">
                {entry.clientName} · {formatTime(entry.atMs)}
                {#if entry.paths.length > 0} · {entry.paths.slice(0, 3).join(', ')}{/if}
                {#if entry.detail} · {entry.detail}{/if}
              </span>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</section>

<style>
  .mcp-server-section {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 0;
    border-top: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
  }

  .section-heading h3 {
    margin: 0 0 4px;
    font-size: 14px;
    font-weight: 600;
  }

  .section-heading p {
    margin: 0;
    max-width: 560px;
    font-size: 12px;
    line-height: 1.5;
    opacity: 0.72;
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
    border-radius: 10px;
  }

  .card h4 {
    margin: 0;
    font-size: 13px;
  }

  .meta {
    margin: 0;
    font-size: 11px;
    opacity: 0.66;
    overflow-wrap: anywhere;
  }

  .switch-line {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    cursor: pointer;
  }

  .switch-line.risk {
    color: var(--danger, #c53f4f);
  }

  .badge {
    padding: 1px 6px;
    border-radius: 999px;
    font-size: 10px;
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    color: var(--accent-strong, #3b6fd4);
  }

  .badge--off {
    background: var(--surface-muted, rgba(127, 127, 127, 0.14));
    color: inherit;
    opacity: 0.8;
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
  }

  .rows li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
  }

  .rows li.detail {
    flex-direction: column;
    align-items: stretch;
    justify-content: flex-start;
    margin-left: 12px;
  }

  .edit-form {
    padding-top: 0;
    border-top: none;
  }

  .rows li.inactive {
    opacity: 0.55;
  }

  .row-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .row-title {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
  }

  .secret-card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border: 1px dashed var(--accent-strong, #3b6fd4);
    border-radius: 8px;
  }

  .secret {
    padding: 6px 8px;
    border-radius: 6px;
    font-size: 12px;
    overflow-wrap: anywhere;
    user-select: all;
    background: var(--surface-muted, rgba(127, 127, 127, 0.12));
  }

  .create-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.14));
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
  }

  .field input,
  .field select {
    padding: 4px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 6px;
    font-size: 12px;
    background: transparent;
    color: inherit;
  }

  .snippet {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .snippet-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .snippet pre {
    margin: 0;
    padding: 8px 10px;
    border-radius: 8px;
    font-size: 11px;
    overflow-x: auto;
    background: var(--surface-muted, rgba(127, 127, 127, 0.1));
  }

  .action-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  button {
    padding: 5px 12px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 8px;
    font-size: 12px;
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  button.primary-action {
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    border-color: transparent;
    color: var(--accent-strong, #3b6fd4);
  }

  button.danger {
    color: var(--danger, #c53f4f);
  }

  button:disabled {
    cursor: default;
    opacity: 0.45;
  }

  .status-message {
    margin: 0;
    font-size: 12px;
  }

  .status-message.error {
    color: var(--danger, #c53f4f);
  }
</style>

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
    clearMcpServerNamedTunnel,
    createMcpServerToken,
    getMcpServerConfigSnippets,
    getMcpServerStatus,
    getMcpServerTokenSecret,
    listMcpServerApprovals,
    resolveMcpServerApproval,
    revokeAllMcpServerTokens,
    revokeMcpServerToken,
    saveMcpServerNamedTunnel,
    rotateMcpServerToken,
    setMcpServerDirect,
    setMcpServerEnabled,
    setMcpServerNetwork,
    updateMcpServerToken,
    verifyMcpServerNamedTunnel,
    type McpServerApproval,
    type McpServerConfigSnippets,
    type McpServerNamedTunnelCheck,
    type McpServerProfile,
    type McpServerStatus,
    type McpServerToken,
  } from '../web/agent-api';
  import { MCP_APPROVALS_CHANGED_EVENT } from '../lib/mcp-server-events';
  import SettingsMcpServerAudit from './SettingsMcpServerAudit.svelte';

  const PROFILES: readonly McpServerProfile[] = ['read_only', 'edit', 'edit_trusted', 'exec'];
  const HIGH_RISK_PROFILES: readonly McpServerProfile[] = ['edit_trusted', 'exec'];
  const APPROVAL_POLL_MS = 3000;

  let status = $state<McpServerStatus | null>(null);
  let snippets = $state<McpServerConfigSnippets | null>(null);
  let approvals = $state<McpServerApproval[]>([]);
  /** 调用记录组件的刷新信号（审批处理后递增）。 */
  let auditVersion = $state(0);

  type McpTab = 'connect' | 'clients' | 'remote' | 'activity';
  /** 当前分组。只是界面状态，不持久化。 */
  let tab = $state<McpTab>('connect');
  let showCreate = $state(false);
  /** 已吊销 / 已过期的令牌默认折叠。 */
  let showInactive = $state(false);
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

  /** 直接对公网开放的表单。 */
  let directBindHost = $state('0.0.0.0');
  let directPublicHosts = $state('');
  let directPort = $state('');

  /** 命名隧道表单。令牌只在提交时经本机接口发给 daemon，提交后立即清空，页面不保留。 */
  let namedHostname = $state('');
  let namedToken = $state('');
  let namedCheck = $state<McpServerNamedTunnelCheck | null>(null);

  /** “连接”分组里配置所用的客户端令牌；空表示保留 <MAGI_MCP_TOKEN> 占位符。 */
  let connectTokenId = $state('');
  /** 用户主动选过“保留占位符”后，不再自动替他选客户端。 */
  let connectTokenTouched = $state(false);

  /** 隧道就绪后的“添加到远程客户端”：选一个公网令牌，得到带令牌的现成配置。 */
  let quickAddTokenId = $state('');
  let quickAddSnippets = $state<McpServerConfigSnippets | null>(null);

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
  const remoteTokens = $derived(
    (status?.tokens ?? []).filter((token) => token.active && token.network),
  );
  const tunnelReady = $derived(
    status?.network.status === 'running' && Boolean(status?.network.mcpUrl),
  );
  /** 远程客户端使用的地址：隧道就绪用隧道地址，否则用直接监听的公网地址。 */
  const remoteMcpUrl = $derived(
    tunnelReady ? (status?.network.mcpUrl ?? null) : (status?.direct.listening ? status.direct.mcpUrl : null),
  );
  const remoteReady = $derived(remoteMcpUrl !== null);
  const directPublicHostList = $derived(
    directPublicHosts.split(/[\s,;]+/).map((host) => host.trim()).filter(Boolean),
  );
  const canStartDirect = $derived(
    !busy
      && hasNetworkToken
      && directBindHost.trim() !== ''
      && directPublicHostList.length > 0
      && (directPort.trim() === '' || /^\d+$/.test(directPort.trim())),
  );
  const quickAddToken = $derived(
    remoteTokens.find((token) => token.tokenId === quickAddTokenId) ?? null,
  );
  const inactiveTokenCount = $derived((status?.tokens ?? []).filter((token) => !token.active).length);
  const visibleTokens = $derived(
    (status?.tokens ?? []).filter((token) => showInactive || token.active),
  );
  /** 能直接填进配置的令牌：有效且保存了原文。 */
  const connectTokens = $derived((status?.tokens ?? []).filter((token) => token.active));
  const TABS: readonly McpTab[] = ['connect', 'clients', 'remote', 'activity'];
  const activeTokenCount = $derived((status?.tokens ?? []).filter((token) => token.active).length);
  function tabBadge(id: McpTab): string {
    if (id === 'clients') return activeTokenCount > 0 ? String(activeTokenCount) : '';
    if (id === 'activity') return approvals.length > 0 ? String(approvals.length) : '';
    if (id === 'remote') return status?.network.enabled || status?.direct.enabled ? '●' : '';
    return '';
  }
  const namedMode = $derived(status?.network.mode === 'named');
  const canSaveNamed = $derived(!busy && namedHostname.trim() !== '' && namedToken.trim() !== '');
  const NETWORK_ERROR_CODES = [
    'tunnel_dependency_unavailable',
    'tunnel_start_failed',
    'tunnel_connection_lost',
    'tunnel_token_invalid',
  ];
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
      approvals = await listMcpServerApprovals();
      auditVersion += 1;
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
      showCreate = false;
      clientName = '';
      confirmHighRisk = false;
      allowNetwork = false;
      await refresh();
    });
  }

  function saveNamedTunnel(): Promise<void> {
    return act(async () => {
      status = await saveMcpServerNamedTunnel(namedHostname.trim(), namedToken.trim());
      namedToken = '';
      namedHostname = '';
      namedCheck = null;
      await refresh();
    });
  }

  function clearNamedTunnel(): Promise<void> {
    if (!window.confirm(i18n.t('mcpServer.named.clearConfirm'))) return Promise.resolve();
    return act(async () => {
      status = await clearMcpServerNamedTunnel();
      namedCheck = null;
      await refresh();
    });
  }

  function checkNamedTunnel(): Promise<void> {
    return act(async () => {
      namedCheck = await verifyMcpServerNamedTunnel();
    });
  }

  /** 配置 JSON 里的 `Authorization` 请求头值（`Bearer <令牌>`）。 */
  function authorizationOf(source: McpServerConfigSnippets): string {
    const servers = (source.remoteJson?.mcpServers ?? {}) as Record<string, { headers?: Record<string, string> }>;
    return servers[source.serverName]?.headers?.Authorization ?? '';
  }

  async function loadConnectSnippets(): Promise<void> {
    if (!status?.running) {
      snippets = null;
      return;
    }
    const token = connectTokens.find((entry) => entry.tokenId === connectTokenId);
    try {
      snippets = await getMcpServerConfigSnippets(token?.hasSecret ? token.tokenId : undefined);
    } catch {
      /* 下一次状态刷新或重新选择客户端时再试 */
    }
  }

  async function loadQuickAdd(): Promise<void> {
    quickAddSnippets = null;
    const token = quickAddToken;
    if (!remoteReady || !token || !token.hasSecret) return;
    try {
      quickAddSnippets = await getMcpServerConfigSnippets(token.tokenId);
    } catch {
      /* 下一次状态刷新或重新选择令牌时再试 */
    }
  }

  function startDirect(): Promise<void> {
    if (!window.confirm(i18n.t('mcpServer.direct.confirm'))) return Promise.resolve();
    return act(async () => {
      const port = directPort.trim();
      status = await setMcpServerDirect({
        enabled: true,
        bindHost: directBindHost.trim(),
        publicHosts: directPublicHostList,
        port: port === '' ? undefined : Number(port),
        confirmRisk: true,
      });
      directPort = '';
      await refresh();
    });
  }

  function stopDirect(): Promise<void> {
    return act(async () => {
      status = await setMcpServerDirect({ enabled: false });
      await refresh();
    });
  }

  /** 已开启时把当前配置带进表单，方便修改后重新应用。 */
  function editDirect(): void {
    if (!status) return;
    directBindHost = status.direct.bindHost;
    directPublicHosts = status.direct.publicHosts.join('\n');
    directPort = status.port ? String(status.port) : '';
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
      auditVersion += 1;
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

  // 默认选一个能直接填进配置的客户端（旧版本创建的令牌没有原文）。
  $effect(() => {
    if (connectTokenId && !connectTokens.some((token) => token.tokenId === connectTokenId)) {
      connectTokenId = '';
    }
    if (!connectTokenId) {
      const first = connectTokens.find((token) => token.hasSecret);
      if (first && !connectTokenTouched) connectTokenId = first.tokenId;
    }
  });

  // 服务状态或所选客户端变化时重新取配置。
  $effect(() => {
    void status?.running;
    void connectTokenId;
    void status?.network.mcpUrl;
    void loadConnectSnippets();
  });

  // 选中的公网令牌失效（吊销、过期）或不存在时，回到一个可用的。
  $effect(() => {
    if (!remoteTokens.some((token) => token.tokenId === quickAddTokenId)) {
      // 优先选能直接复制的令牌（旧版本创建的令牌没有原文）。
      quickAddTokenId = (remoteTokens.find((token) => token.hasSecret) ?? remoteTokens[0])?.tokenId ?? '';
    }
  });

  // 隧道就绪、地址变化或令牌变化时重新取配置。
  $effect(() => {
    void remoteMcpUrl;
    void quickAddTokenId;
    void loadQuickAdd();
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

  <div class="tabs" role="tablist" data-mcp-tabs="1">
    {#each TABS as id (id)}
      <button
        type="button"
        role="tab"
        class="tab"
        class:tab--active={tab === id}
        aria-selected={tab === id}
        data-mcp-tab={id}
        onclick={() => { tab = id; }}
      >
        {i18n.t(`mcpServer.tab.${id}`)}
        {#if tabBadge(id)}<span class="tab-badge" class:tab-badge--alert={id === 'activity'}>{tabBadge(id)}</span>{/if}
      </button>
    {/each}
  </div>

  {#if tab === 'connect'}
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

  {/if}

  {#if tab === 'remote'}
  <div class="card" data-mcp-server-network="1">
    <h4>{i18n.t('mcpServer.network.section')}</h4>
    <p class="meta">{i18n.t('mcpServer.network.desc')}</p>
    <p class="meta" data-mcp-network-mode={status?.network.mode ?? 'quick'}>
      {namedMode
        ? i18n.t('mcpServer.named.modeNamed', { hostname: status?.network.namedHostname ?? '' })
        : i18n.t('mcpServer.network.addressChanges')}
    </p>
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
      <div class="action-row">
        <button type="button" class="primary-action" data-mcp-go-clients-network="1" onclick={() => { tab = 'clients'; showCreate = true; allowNetwork = true; }}>
          {i18n.t('mcpServer.snippets.goCreate')}
        </button>
      </div>
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
      <p class="status-message error" role="alert">
        {NETWORK_ERROR_CODES.includes(status.network.error)
          ? i18n.t(`mcpServer.network.error.${status.network.error}`)
          : status.network.error}
      </p>
    {/if}
    <p class="meta">{i18n.t('mcpServer.network.autoStop')}</p>

    {#if remoteReady}
      <div class="quick-add" data-mcp-quick-add="1">
        <h4>{i18n.t('mcpServer.quickAdd.section')}</h4>
        <p class="meta">{i18n.t('mcpServer.quickAdd.desc')}</p>
        {#if remoteTokens.length === 0}
          <p class="meta">{i18n.t('mcpServer.network.needToken')}</p>
          <div class="action-row">
            <button type="button" class="primary-action" onclick={() => { tab = 'clients'; showCreate = true; allowNetwork = true; }}>
              {i18n.t('mcpServer.snippets.goCreate')}
            </button>
          </div>
        {:else}
          <label class="field">
            <span>{i18n.t('mcpServer.quickAdd.token')}</span>
            <select bind:value={quickAddTokenId} data-mcp-quick-add-token="1">
              {#each remoteTokens as token (token.tokenId)}
                <option value={token.tokenId}>{token.clientName} · {token.prefix}…</option>
              {/each}
            </select>
          </label>
          {#if quickAddToken && !quickAddToken.hasSecret}
            <p class="meta" data-mcp-quick-add-legacy="1">{i18n.t('mcpServer.snippets.legacyHint')}</p>
          {:else if quickAddSnippets?.remoteJson}
            {@const secretValue = authorizationOf(quickAddSnippets)}
            <div class="snippet">
              <div class="snippet-head">
                <span class="row-title">{i18n.t('mcpServer.quickAdd.form')}</span>
              </div>
              {#each [
                { key: 'qa-url', label: i18n.t('mcpServer.quickAdd.fieldUrl'), value: remoteMcpUrl ?? '' },
                { key: 'qa-header', label: i18n.t('mcpServer.quickAdd.fieldHeaderName'), value: 'Authorization' },
                { key: 'qa-value', label: i18n.t('mcpServer.quickAdd.fieldHeaderValue'), value: secretValue },
              ] as field (field.key)}
                <div class="form-field-row">
                  <span class="meta">{field.label}</span>
                  <code class="secret" data-mcp-quick-add-field={field.key}>{field.value}</code>
                  <button type="button" onclick={() => void copy(field.key, field.value)}>
                    {copiedKey === field.key ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
                  </button>
                </div>
              {/each}
            </div>
            {#each [
              { key: 'qa-json', label: i18n.t('mcpServer.quickAdd.json'), text: JSON.stringify(quickAddSnippets.remoteJson, null, 2) },
              ...(quickAddSnippets.remoteClaudeCli
                ? [{ key: 'qa-cli', label: i18n.t('mcpServer.snippets.claudeCli'), text: quickAddSnippets.remoteClaudeCli }]
                : []),
            ] as entry (entry.key)}
              <div class="snippet">
                <div class="snippet-head">
                  <span class="row-title">{entry.label}</span>
                  <button type="button" data-mcp-quick-add-copy={entry.key} onclick={() => void copy(entry.key, entry.text)}>
                    {copiedKey === entry.key ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
                  </button>
                </div>
                <pre>{entry.text}</pre>
              </div>
            {/each}
            <p class="meta">{!tunnelReady ? i18n.t('mcpServer.direct.plainHint') : status?.network.mode === 'named' ? i18n.t('mcpServer.quickAdd.fixedHint') : i18n.t('mcpServer.network.addressChanges')}</p>
          {/if}
        {/if}
      </div>
    {/if}

    <div class="named-tunnel" data-mcp-named-tunnel="1">
      <h4>{i18n.t('mcpServer.named.section')}</h4>
      <p class="meta">{i18n.t('mcpServer.named.desc')}</p>
      <p class="meta" data-mcp-named-assist="1">{i18n.t('mcpServer.named.assist')}</p>
      {#if namedMode}
        <p class="meta"><span class="badge">{i18n.t('mcpServer.named.configured', { hostname: status?.network.namedHostname ?? '' })}</span></p>
        <p class="meta">{i18n.t('mcpServer.named.tokenHidden')} {i18n.t('mcpServer.named.autoRestore')}</p>
        <div class="action-row">
          <button type="button" data-mcp-named-verify="1" disabled={busy} onclick={() => void checkNamedTunnel()}>
            {i18n.t('mcpServer.named.verify')}
          </button>
          <button type="button" class="danger" disabled={busy} onclick={() => void clearNamedTunnel()}>
            {i18n.t('mcpServer.named.clear')}
          </button>
        </div>
        {#if namedCheck}
          <p class="status-message" class:error={!namedCheck.ok} role="status" data-mcp-named-check={namedCheck.code}>
            {i18n.t(`mcpServer.named.verifyResult.${namedCheck.code}`, { port: String(status?.port ?? ''), status: String(namedCheck.httpStatus ?? '') })}
          </p>
        {/if}
      {/if}
      <form class="create-form edit-form" onsubmit={(event) => { event.preventDefault(); void saveNamedTunnel(); }}>
        <label class="field">
          <span>{i18n.t('mcpServer.named.hostname')}</span>
          <input bind:value={namedHostname} placeholder="mcp.example.com" autocomplete="off" data-mcp-named-hostname="1" />
        </label>
        <label class="field">
          <span>{i18n.t('mcpServer.named.token')}</span>
          <input type="password" bind:value={namedToken} placeholder={i18n.t('mcpServer.named.tokenPlaceholder')} autocomplete="off" data-mcp-named-token="1" />
        </label>
        <p class="meta">{i18n.t('mcpServer.named.portHint', { port: String(status?.port ?? '') })}</p>
        <div class="action-row">
          <button type="submit" class="primary-action" disabled={!canSaveNamed}>
            {namedMode ? i18n.t('mcpServer.named.replace') : i18n.t('mcpServer.named.save')}
          </button>
        </div>
      </form>
    </div>
  </div>

  <div class="card" data-mcp-server-direct="1">
    <h4>{i18n.t('mcpServer.direct.section')}</h4>
    <p class="meta">{i18n.t('mcpServer.direct.desc')}</p>
    <p class="meta risk-note" data-mcp-direct-risk="1">{i18n.t('mcpServer.direct.risk')}</p>
    {#if status?.direct.error}
      <p class="status-message error" role="alert">{i18n.t('mcpServer.direct.restoreFailed', { message: status.direct.error })}</p>
    {/if}
    {#if status?.direct.enabled}
      <p class="meta">
        <span class="badge" class:badge--off={!status.direct.listening}>
          {status.direct.listening ? i18n.t('mcpServer.direct.listening') : i18n.t('mcpServer.direct.notListening')}
        </span>
        {i18n.t('mcpServer.direct.bind', { host: status.direct.bindHost, port: String(status.port ?? '') })}
      </p>
      {#if status.direct.mcpUrl}
        <div class="snippet-head">
          <span class="meta">{i18n.t('mcpServer.direct.url')}</span>
          <button type="button" onclick={() => void copy('direct-url', status?.direct.mcpUrl ?? '')}>
            {copiedKey === 'direct-url' ? i18n.t('mcpServer.token.copied') : i18n.t('mcpServer.token.copy')}
          </button>
        </div>
        <code class="secret" data-mcp-direct-url="1">{status.direct.mcpUrl}</code>
      {/if}
      <p class="meta">{i18n.t('mcpServer.direct.firewall', { port: String(status.port ?? '') })}</p>
      <p class="meta">{i18n.t('mcpServer.direct.autoRestore')}</p>
      <div class="action-row">
        <button type="button" class="danger" data-mcp-direct-stop="1" disabled={busy} onclick={() => void stopDirect()}>
          {i18n.t('mcpServer.direct.stop')}
        </button>
        <button type="button" disabled={busy} onclick={editDirect}>{i18n.t('mcpServer.direct.edit')}</button>
      </div>
    {/if}
    {#if !hasNetworkToken}
      <p class="meta">{i18n.t('mcpServer.network.needToken')}</p>
    {/if}
    <form class="create-form edit-form" onsubmit={(event) => { event.preventDefault(); void startDirect(); }}>
      <label class="field">
        <span>{i18n.t('mcpServer.direct.publicHosts')}</span>
        <textarea rows="2" bind:value={directPublicHosts} placeholder="203.0.113.5&#10;mcp.example.com" data-mcp-direct-hosts="1"></textarea>
      </label>
      <label class="field">
        <span>{i18n.t('mcpServer.direct.bindHost')}</span>
        <input bind:value={directBindHost} autocomplete="off" data-mcp-direct-bind="1" />
      </label>
      <p class="meta">{i18n.t('mcpServer.direct.bindHint')}</p>
      <label class="field">
        <span>{i18n.t('mcpServer.direct.port')}</span>
        <input bind:value={directPort} inputmode="numeric" placeholder={status?.port ? String(status.port) : ''} data-mcp-direct-port="1" />
      </label>
      <div class="action-row">
        <button type="submit" class="primary-action" data-mcp-direct-start="1" disabled={!canStartDirect}>
          {status?.direct.enabled ? i18n.t('mcpServer.direct.apply') : i18n.t('mcpServer.direct.start')}
        </button>
      </div>
    </form>
  </div>

  {/if}

  {#if tab === 'activity'}
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

  {/if}

  {#if tab === 'clients'}
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
        {#each visibleTokens as token (token.tokenId)}
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
      {#if inactiveTokenCount > 0}
        <div class="action-row">
          <button type="button" data-mcp-toggle-inactive="1" onclick={() => { showInactive = !showInactive; }}>
            {showInactive
              ? i18n.t('mcpServer.token.hideInactive')
              : i18n.t('mcpServer.token.showInactive', { count: String(inactiveTokenCount) })}
          </button>
        </div>
      {/if}
      {#if status.tokens.some((token) => token.active)}
        <div class="action-row">
          <button type="button" class="danger" disabled={busy} onclick={() => void revokeAll()}>
            {i18n.t('mcpServer.token.revokeAll')}
          </button>
        </div>
      {/if}
    {/if}

    {#if !showCreate}
      <div class="action-row">
        <button type="button" class="primary-action" data-mcp-create-open="1" onclick={() => { showCreate = true; }}>
          {i18n.t('mcpServer.token.new')}
        </button>
      </div>
    {:else}
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
        <button type="button" onclick={() => { showCreate = false; }}>{i18n.t('mcpServer.token.cancel')}</button>
      </div>
    </form>
    {/if}
  </div>

  {/if}

  {#if tab === 'connect'}
  <div class="card" data-mcp-server-snippets="1">
    <h4>{i18n.t('mcpServer.snippets.section')}</h4>
    <p class="meta" data-mcp-token-origin="1">{i18n.t('mcpServer.snippets.hint')}</p>
    {#if connectTokens.length === 0}
      <p class="meta" data-mcp-no-tokens="1">{i18n.t('mcpServer.snippets.noTokens')}</p>
      <div class="action-row">
        <button type="button" class="primary-action" data-mcp-go-clients="1" onclick={() => { tab = 'clients'; showCreate = true; }}>
          {i18n.t('mcpServer.snippets.goCreate')}
        </button>
      </div>
    {:else}
      <label class="field">
        <span>{i18n.t('mcpServer.snippets.useToken')}</span>
        <select
          bind:value={connectTokenId}
          onchange={() => { connectTokenTouched = true; }}
          data-mcp-connect-token="1"
        >
          <option value="">{i18n.t('mcpServer.snippets.placeholderOption')}</option>
          {#each connectTokens as token (token.tokenId)}
            <option value={token.tokenId} disabled={!token.hasSecret}>
              {token.clientName} · {token.prefix}…{token.hasSecret ? '' : ` (${i18n.t('mcpServer.snippets.noSecretOption')})`}
            </option>
          {/each}
        </select>
      </label>
      {#if connectTokens.some((token) => !token.hasSecret)}
        <p class="meta">{i18n.t('mcpServer.snippets.legacyHint')}</p>
      {/if}
    {/if}
    {#if !snippets}
      <p class="meta">{i18n.t('mcpServer.snippets.needStart')}</p>
    {:else}
      {#if !snippets.secretFilled}
        <p class="meta" data-mcp-placeholder-note="1">{i18n.t('mcpServer.snippets.placeholderNote')}</p>
      {:else}
        <p class="meta">{i18n.t('mcpServer.snippets.perClientHint', { name: snippets.serverName })}</p>
      {/if}
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
      <p class="meta">{i18n.t('mcpServer.snippets.stdioNote')}</p>
    {/if}
  </div>

  {/if}

  {#if tab === 'activity'}
    <SettingsMcpServerAudit tokens={status?.tokens ?? []} version={auditVersion} />
  {/if}
</section>

<style>
  .mcp-server-section {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 0;
    border-top: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
  }

  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 3px;
    border-radius: 10px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.1));
    align-self: flex-start;
  }

  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 14px;
    border: none;
    border-radius: 8px;
    font-size: 12px;
    background: transparent;
    color: inherit;
    opacity: 0.72;
    cursor: pointer;
  }

  .tab--active {
    background: var(--surface, rgba(255, 255, 255, 0.14));
    font-weight: 600;
    opacity: 1;
  }

  .tab-badge {
    min-width: 16px;
    padding: 0 5px;
    border-radius: 999px;
    font-size: 10px;
    line-height: 16px;
    text-align: center;
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    color: var(--accent-strong, #3b6fd4);
  }

  .tab-badge--alert {
    background: var(--danger, #c53f4f);
    color: #fff;
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

  .quick-add {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 10px;
    border-top: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.14));
  }

  .form-field-row {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
  }

  .risk-note {
    color: var(--danger, #c53f4f);
    opacity: 0.9;
  }

  .field textarea {
    padding: 4px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 6px;
    font-size: 12px;
    font-family: inherit;
    background: transparent;
    color: inherit;
    resize: vertical;
  }

  .named-tunnel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 10px;
    border-top: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.14));
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

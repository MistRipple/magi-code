<script lang="ts">
  import Disclosure from './Disclosure.svelte';
  import Icon from './Icon.svelte';
  import SelectField from './SelectField.svelte';
  import Toggle from './Toggle.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { MCPServer } from '../stores/settings-store.svelte';

  let {
    servers, hydrated, loading, listError, retryServers, expandedServer, tools, refreshing, errors, changing,
    openEditor, toggleExpanded, healthLabel, toggleServer, deleteServer, refreshTools,
  }: {
    servers: MCPServer[];
    hydrated: boolean;
    loading: boolean;
    listError: string;
    retryServers: () => void;
    expandedServer: string | null;
    tools: Record<string, Array<{ name: string; description?: string }>>;
    refreshing: Set<string>;
    errors: Record<string, string>;
    changing: Set<string>;
    openEditor: (server: MCPServer | null) => void;
    toggleExpanded: (id: string) => void;
    healthLabel: (server: MCPServer) => string;
    toggleServer: (id: string, enabled: boolean) => void;
    deleteServer: (id: string) => void;
    refreshTools: (id: string) => void;
  } = $props();

  let query = $state('');
  let filter = $state<'all' | 'enabled' | 'issue'>('all');
  let searchCollapsed = $state<{ query: string; ids: Set<string> }>({ query: '', ids: new Set() });
  const search = $derived(query.trim().toLocaleLowerCase(i18n.locale));
  function matchesTool(tool: { name: string; description?: string }): boolean {
    return `${tool.name} ${tool.description ?? ''}`.toLocaleLowerCase(i18n.locale).includes(search);
  }
  function matchesServer(server: MCPServer): boolean {
    return server.name.toLocaleLowerCase(i18n.locale).includes(search);
  }
  const visibleServers = $derived(servers.filter(server => {
    if (filter === 'enabled' && !server.enabled) return false;
    if (filter === 'issue' && (!server.enabled || (!server.error && !errors[server.id] && server.health !== 'degraded'))) return false;
    return !search || matchesServer(server) || tools[server.id]?.some(matchesTool);
  }));
  function visibleTools(server: MCPServer) {
    const list = tools[server.id] ?? [];
    if (!search) return list;
    const score = (name: string) => {
      const value = name.toLocaleLowerCase(i18n.locale);
      return value === search ? 0 : value.startsWith(search) ? 1 : value.includes(search) ? 2 : 3;
    };
    return (matchesServer(server) ? [...list] : list.filter(matchesTool))
      .sort((left, right) => score(left.name) - score(right.name));
  }
  function isExpanded(server: MCPServer): boolean {
    if (search && tools[server.id]?.some(matchesTool)) {
      return searchCollapsed.query !== search || !searchCollapsed.ids.has(server.id);
    }
    return expandedServer === server.id;
  }
  function toggleDirectory(server: MCPServer): void {
    if (search && tools[server.id]?.some(matchesTool)) {
      const ids = new Set(searchCollapsed.query === search ? searchCollapsed.ids : []);
      if (isExpanded(server)) ids.add(server.id);
      else ids.delete(server.id);
      searchCollapsed = { query: search, ids };
    } else toggleExpanded(server.id);
  }
  function toolCount(server: MCPServer): string {
    // Cached directories remain useful for inspection, including while the service is disabled.
    const count = tools[server.id]?.length ?? (server.connected ? server.toolCount : undefined);
    return typeof count === 'number' ? i18n.t('settings.tools.mcpToolCount', { count }) : '—';
  }
  function errorMessage(server: MCPServer): string {
    return errors[server.id] || (server.enabled && server.error ? i18n.t('settings.tools.mcpDirectoryFailed') : '');
  }
</script>

<section class="settings-section mcp-section">
  <div class="settings-section-header">
    <div class="mcp-heading">
      <h3 class="settings-section-title">{i18n.t('settings.tools.mcpTools')}</h3>
      <span class="mcp-meta">{i18n.t('settings.tools.mcpServerCount', { count: servers.length })}</span>
    </div>
    <div class="settings-section-actions">
      <button type="button" class="btn btn--primary btn--sm" onclick={() => openEditor(null)}>
        <Icon name="plus" size={16} />{i18n.t('settings.tools.addServer')}
      </button>
    </div>
  </div>

  {#if servers.length > 0}
    <div class="mcp-toolbar">
      <div class="mcp-search">
        <Icon name="search" size={16} />
        <input class="form-input" type="search" bind:value={query}
          placeholder={i18n.t('settings.tools.mcpSearch')}
          aria-label={i18n.t('settings.tools.mcpSearch')} />
      </div>
      <SelectField bind:value={filter} label={i18n.t('settings.tools.mcpFilter')}
        options={[
          { value: 'all', label: i18n.t('settings.tools.mcpFilterAll') },
          { value: 'enabled', label: i18n.t('settings.tools.mcpFilterEnabled') },
          { value: 'issue', label: i18n.t('settings.tools.mcpFilterIssue') },
        ]} />
    </div>
  {/if}

  {#if listError}
    <div class="settings-notice mcp-failure" role="alert">
      <span>{listError}</span>
      <button type="button" class="btn btn--secondary btn--sm" disabled={loading} onclick={retryServers}>{i18n.t('settings.tools.mcpRetry')}</button>
    </div>
  {/if}
  {#if loading && !hydrated}
    <div class="settings-empty" role="status"><Icon name="loader" size={16} class="spinning" />{i18n.t('settings.tools.loading')}</div>
  {:else if servers.length === 0 && !listError}
    <div class="settings-empty">{i18n.t('settings.tools.noMcpServerHint')}</div>
  {:else if servers.length > 0 && visibleServers.length === 0}
    <div class="settings-empty" role="status">{i18n.t('settings.tools.mcpNoMatches')}</div>
  {:else if servers.length > 0}
    <div class="settings-card mcp-server-list">
      <div class="mcp-columns mcp-list-header" aria-hidden="true">
        <span>{i18n.t('settings.tools.mcpColumnServer')}</span>
        <span>{i18n.t('settings.tools.mcpColumnStatus')}</span>
        <span>{i18n.t('settings.tools.mcpColumnTools')}</span>
        <span class="mcp-management-label">{i18n.t('settings.tools.mcpColumnActions')}</span>
      </div>
      {#each visibleServers as server (server.id)}
        {@const expanded = isExpanded(server)}
        {@const failure = errorMessage(server)}
        {@const cached = tools[server.id] !== undefined}
        <div class="mcp-server-item" class:expanded data-mcp-server={server.id}>
          <div class="mcp-columns mcp-server-row">
            <button type="button" class="settings-disclosure-trigger mcp-server-primary" onclick={() => toggleDirectory(server)}
              aria-expanded={expanded} aria-controls={`mcp-directory-${server.id}`}>
              <span class="settings-disclosure-chevron"><Icon name="chevron-right" size={14} /></span>
              <span class="settings-row-copy">
                <span class="settings-row-title mcp-server-name" title={server.name}>{server.name}</span>
                <span class="settings-row-desc">{server.type === 'stdio' ? 'stdio' : 'HTTP'}</span>
              </span>
            </button>
            <span class="mcp-health" class:mcp-health--ok={server.enabled && server.health === 'connected'}
              class:mcp-health--error={server.enabled && Boolean(server.error)}>
              <span class="settings-dot"></span>{healthLabel(server)}
            </span>
            <span class="mcp-meta mcp-tool-count">{toolCount(server)}</span>
            <div class="mcp-server-actions">
              <button type="button" class="btn-icon btn-icon--sm" aria-label={i18n.t('settings.tools.refreshToolsFor', { name: server.name })}
                title={i18n.t('settings.tools.refreshTools')} aria-busy={refreshing.has(server.id)}
                disabled={!server.enabled || refreshing.has(server.id) || changing.has(server.id)} onclick={() => refreshTools(server.id)}>
                <Icon name={refreshing.has(server.id) ? 'loader' : 'refresh'} size={16} class={`tools-refresh-icon ${refreshing.has(server.id) ? 'spinning' : ''}`} />
              </button>
              <Toggle checked={server.enabled} disabled={changing.has(server.id) || refreshing.has(server.id)}
                ariaLabel={i18n.t('settings.tools.mcpEnableServer', { name: server.name })}
                title={server.enabled ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
                onchange={() => toggleServer(server.id, server.enabled)} />
              <button type="button" class="btn-icon btn-icon--sm" aria-label={i18n.t('settings.tools.mcpEditServer', { name: server.name })}
                title={i18n.t('settings.tools.edit')} disabled={changing.has(server.id) || refreshing.has(server.id)} onclick={() => openEditor(server)}>
                <Icon name="edit" size={16} />
              </button>
              <button type="button" class="btn-icon btn-icon--sm btn-icon--danger" aria-label={i18n.t('settings.tools.mcpDeleteServer', { name: server.name })}
                title={i18n.t('settings.tools.delete')} disabled={changing.has(server.id)} onclick={() => deleteServer(server.id)}>
                <Icon name="trash" size={16} />
              </button>
            </div>
          </div>
          <Disclosure {expanded}>
            <div class="mcp-tools-list" id={`mcp-directory-${server.id}`}>
              {#if refreshing.has(server.id) && !cached}
                <div class="mcp-message" role="status"><Icon name="loader" size={16} class="spinning" />{i18n.t('settings.tools.loading')}</div>
              {/if}
              {#if failure}
                <div class="settings-notice mcp-failure" role="alert">
                  <div class="mcp-error-copy">
                    <p>{failure}</p>
                    {#if cached}<p class="mcp-meta">{i18n.t('settings.tools.mcpCachedDirectory')}</p>{/if}
                    {#if server.errorDetail && server.errorDetail !== failure}
                      <details><summary class="settings-disclosure-trigger"><span class="settings-disclosure-chevron"><Icon name="chevron-right" size={14} /></span>{i18n.t('settings.tools.mcpConnectionDetail')}</summary><pre>{server.errorDetail}</pre></details>
                    {/if}
                  </div>
                  <button type="button" class="btn btn--secondary btn--sm" disabled={!server.enabled || refreshing.has(server.id) || changing.has(server.id)}
                    onclick={() => refreshTools(server.id)}>{i18n.t('settings.tools.mcpRetry')}</button>
                </div>
              {:else if !server.enabled}
                <p class="mcp-message mcp-meta">{i18n.t(cached ? 'settings.tools.mcpDisabledCached' : 'settings.tools.mcpDisabledDirectory')}</p>
              {:else if cached && !server.connected}
                <p class="mcp-message mcp-meta">{i18n.t('settings.tools.mcpCachedDirectory')}</p>
              {/if}
              {#if cached}
                {#each visibleTools(server) as tool (tool.name)}
                  {@const description = tool.description?.trim() ?? ''}
                  <div class="mcp-tool-row">
                    <code class="mcp-tool-name">{tool.name}</code>
                    <div class="mcp-tool-copy">
                      {#if description.length > 160}
                        <details class="mcp-description">
                          <summary class="settings-disclosure-trigger">
                            <span class="settings-disclosure-chevron"><Icon name="chevron-right" size={14} /></span>
                            <span class="mcp-description-preview">{description.slice(0, 140)}…</span>
                            <span class="mcp-description-more">{i18n.t('settings.tools.mcpExpandDescription')}</span>
                            <span class="mcp-description-less">{i18n.t('settings.tools.mcpCollapseDescription')}</span>
                          </summary>
                          <p>{description}</p>
                        </details>
                      {:else}
                        <p>{description || i18n.t('settings.tools.mcpNoDescription')}</p>
                      {/if}
                    </div>
                  </div>
                {:else}
                  {#if !failure && server.enabled && !refreshing.has(server.id)}
                    <p class="mcp-message mcp-meta">{i18n.t(search ? 'settings.tools.mcpNoMatches' : 'settings.tools.mcpDirectoryEmpty')}</p>
                  {/if}
                {/each}
              {/if}
            </div>
          </Disclosure>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .mcp-section { min-inline-size: 0; }
  .mcp-heading { display: flex; align-items: center; flex-wrap: wrap; gap: var(--space-3); }
  .mcp-meta { font-size: var(--text-xs); color: var(--foreground-muted); }
  .mcp-toolbar { display: flex; gap: var(--space-3); margin-block-end: var(--space-4); }
  .mcp-search { position: relative; flex: 1; min-inline-size: 0; }
  .mcp-search > :global(svg) { position: absolute; inset-inline-start: 10px; inset-block-start: 10px; color: var(--foreground-muted); pointer-events: none; }
  .mcp-search input { padding-inline-start: 34px; inline-size: 100%; }
  .mcp-server-list { padding: 0; }
  .mcp-columns { display: grid; grid-template-columns: minmax(0, 1fr) minmax(80px, 110px) minmax(68px, 86px) 156px; gap: var(--space-3); align-items: center; }
  .mcp-list-header { padding: 10px var(--space-4); border-block-end: 1px solid var(--border); font-size: var(--text-xs); color: var(--foreground-muted); }
  .mcp-management-label { text-align: end; }
  .mcp-server-item + .mcp-server-item { border-block-start: 1px solid var(--border); }
  .mcp-server-row { min-block-size: 56px; padding: var(--space-3) var(--space-4); }
  .mcp-server-primary { inline-size: 100%; }
  .mcp-server-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .mcp-health { display: flex; align-items: center; gap: 6px; min-inline-size: 0; font-size: var(--text-xs); color: var(--foreground-muted); }
  .mcp-health .settings-dot { background: currentColor; }
  .mcp-health--ok { color: var(--success-text); }
  .mcp-health--error { color: var(--error-text); }
  .mcp-tool-count { overflow-wrap: anywhere; }
  .mcp-server-actions { display: flex; justify-content: flex-end; align-items: center; gap: 4px; }
  .mcp-tools-list { padding-block: 0 var(--space-4); padding-inline: 40px var(--space-4); }
  .mcp-tool-row { display: grid; grid-template-columns: minmax(100px, 200px) minmax(0, 1fr); align-items: baseline; gap: 20px; padding-block: 10px; }
  .mcp-tool-row + .mcp-tool-row { border-block-start: 1px solid var(--border); }
  .mcp-tool-name { font: 500 var(--text-xs)/1.6 var(--font-mono); direction: ltr; unicode-bidi: isolate; overflow-wrap: anywhere; }
  .mcp-tool-copy { min-inline-size: 0; font-size: var(--text-xs); color: var(--foreground-muted); line-height: 1.6; overflow-wrap: anywhere; }
  .mcp-tool-copy p, .mcp-error-copy p { margin: 0; white-space: pre-wrap; }
  .mcp-description-preview { flex: 1; min-inline-size: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-description-more, .mcp-description-less { flex-shrink: 0; white-space: nowrap; text-decoration: underline; text-underline-offset: 3px; }
  .mcp-description-less { display: none; }
  .mcp-description[open] .mcp-description-preview, .mcp-description[open] .mcp-description-more { display: none; }
  .mcp-description[open] .mcp-description-less { display: inline; }
  .mcp-description[open] p { margin-block-start: 4px; }
  .mcp-message { display: flex; align-items: center; gap: var(--space-3); margin: 0; padding-block: var(--space-4); }
  .mcp-failure { display: flex; justify-content: space-between; align-items: start; gap: var(--space-3); }
  .mcp-error-copy { min-inline-size: 0; }
  .mcp-error-copy > p + p { margin-block-start: var(--space-2); }
  .mcp-error-copy details { margin-block-start: var(--space-3); }
  .mcp-error-copy pre { white-space: pre-wrap; overflow-wrap: anywhere; font: var(--text-xs)/1.6 var(--font-mono); margin-block: var(--space-3) 0; }
  @container settings-content (max-width: 720px) {
    .mcp-list-header { display: none; }
    .mcp-server-row { grid-template-columns: minmax(0, 1fr) auto; gap: var(--space-2) var(--space-3); }
    .mcp-server-primary { grid-column: 1 / -1; }
    .mcp-health { grid-column: 1; grid-row: 2; }
    .mcp-tool-count { grid-column: 1; grid-row: 3; }
    .mcp-server-actions { grid-column: 2; grid-row: 2 / 4; }
    .mcp-tools-list { padding-inline-start: var(--space-4); }
    .mcp-tool-row { grid-template-columns: minmax(0, 1fr); gap: var(--space-2); }
    .mcp-toolbar { flex-wrap: wrap; }
    .mcp-search { flex-basis: 200px; }
  }
  @media (pointer: coarse) {
    .mcp-server-actions { gap: 4px; }
    .mcp-server-actions :global(.btn-icon), .mcp-server-actions :global(.toggle) { min-inline-size: 44px; min-block-size: 44px; }
  }
</style>

<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';

  let {
    mcpServers,
    mcpServersHydrated,
    mcpServersLoading,
    mcpExpandedServer,
    mcpServerTools,
    mcpRefreshingServers,
    builtinTools,
    skills,
    openMCPDialog,
    toggleMCPExpand,
    getMCPHealthLabel,
    toggleMCPServer,
    deleteMCPServer,
    refreshMCPTools,
    openSkillLibraryDialog,
    openRepoDialog,
    deleteSkill
  } = $props<{
    mcpServers: any[];
    mcpServersHydrated: boolean;
    mcpServersLoading: boolean;
    mcpExpandedServer: string | null;
    mcpServerTools: Record<string, any[]>;
    mcpRefreshingServers: Set<string>;
    builtinTools: Array<{
      name: string;
      riskLevel: string;
      approvalRequirement: string;
      accessMode: string;
      runtimeStatus: string;
      runtimeWarnings: string[];
      schemaStatus: string;
      schemaWarnings: string[];
      enabled: boolean;
    }>;
    skills: any[];
    openMCPDialog: (server: any) => void;
    toggleMCPExpand: (sid: string) => void;
    getMCPHealthLabel: (server: any) => string;
    toggleMCPServer: (sid: string, enabled: boolean) => void;
    deleteMCPServer: (sid: string) => void;
    refreshMCPTools: (sid: string) => void;
    openSkillLibraryDialog: () => void;
    openRepoDialog: () => void;
    deleteSkill: (skill: any) => void;
  }>();

  function builtinToolI18nKey(name: string): string {
    const suffix = name
      .split('_')
      .filter(Boolean)
      .map((part, index) => index === 0 ? part : part.charAt(0).toUpperCase() + part.slice(1))
      .join('');
    return suffix ? `settings.tools.builtin.${suffix}` : '';
  }

  function getBuiltinToolLabel(name: string): string {
    const key = builtinToolI18nKey(name);
    return key ? i18n.t(key) : name;
  }

  function getBuiltinToolAccessLabel(accessMode: string): string {
    switch (accessMode) {
      case 'explicit_write':
        return i18n.t('settings.tools.accessWrite');
      case 'maybe_write':
        return i18n.t('settings.tools.accessMaybeWrite');
      default:
        return i18n.t('settings.tools.accessReadOnly');
    }
  }

  function getBuiltinToolRiskLabel(riskLevel: string): string {
    switch (riskLevel.toLowerCase()) {
      case 'high':
        return i18n.t('settings.tools.riskHigh');
      case 'medium':
        return i18n.t('settings.tools.riskMedium');
      case 'low':
        return i18n.t('settings.tools.riskLow');
      default:
        return riskLevel || i18n.t('settings.tools.unknown');
    }
  }

  function getBuiltinToolRuntimeLabel(status: string): string {
    switch (status) {
      case 'ready':
        return i18n.t('settings.tools.runtimeReady');
      case 'not_ready':
        return i18n.t('settings.tools.runtimeNotReady');
      case 'missing_context':
        return i18n.t('settings.tools.runtimeMissingContext');
      case 'unavailable':
        return i18n.t('settings.tools.runtimeUnavailable');
      default:
        return status || i18n.t('settings.tools.unknown');
    }
  }

  function getBuiltinToolRuntimeClass(status: string): string {
    switch (status) {
      case 'ready':
        return 'success';
      case 'not_ready':
      case 'missing_context':
        return 'warning';
      case 'unavailable':
        return 'error';
      default:
        return 'disabled';
    }
  }

  function getBuiltinToolRuntimeTitle(tool: {
    runtimeStatus: string;
    runtimeWarnings: string[];
    schemaWarnings: string[];
  }): string {
    const parts = [getBuiltinToolRuntimeLabel(tool.runtimeStatus)];
    if (tool.runtimeWarnings.length > 0) parts.push(...tool.runtimeWarnings);
    if (tool.schemaWarnings.length > 0) parts.push(...tool.schemaWarnings);
    return parts.join('\n');
  }

  let builtinExpanded = $state(false);
  const builtinReadyCount = $derived(
    (builtinTools as Array<{ runtimeStatus: string }>).filter((t) => t.runtimeStatus === 'ready').length,
  );

  // MCP 工具描述 hover 浮层：默认右侧弹出；按钮 + 浮层共同构成 hover 区域，移出即消失
  const DESC_POPOVER_WIDTH = 320;
  const DESC_POPOVER_MAX_HEIGHT = 240;
  const DESC_CLOSE_GRACE_MS = 120;
  let descAnchor = $state<{ x: number; y: number; description: string } | null>(null);
  let closeTimer: ReturnType<typeof setTimeout> | null = null;

  function clearCloseTimer() {
    if (closeTimer !== null) {
      clearTimeout(closeTimer);
      closeTimer = null;
    }
  }

  function scheduleClose() {
    clearCloseTimer();
    closeTimer = setTimeout(() => {
      descAnchor = null;
      closeTimer = null;
    }, DESC_CLOSE_GRACE_MS);
  }

  function openDesc(description: string, e: MouseEvent) {
    clearCloseTimer();
    const btn = e.currentTarget as HTMLElement;
    const rect = btn.getBoundingClientRect();
    const gutter = 8;
    // 默认按钮右侧弹出；右侧不够则回落到左侧
    let left = rect.right + 8;
    if (left + DESC_POPOVER_WIDTH + gutter > window.innerWidth) {
      left = rect.left - DESC_POPOVER_WIDTH - 8;
    }
    if (left < gutter) left = gutter;
    // 垂直对齐按钮中线，再按底部/顶部边界 clamp
    const btnCenter = rect.top + rect.height / 2;
    let top = btnCenter - DESC_POPOVER_MAX_HEIGHT / 2;
    if (top + DESC_POPOVER_MAX_HEIGHT + gutter > window.innerHeight) {
      top = window.innerHeight - DESC_POPOVER_MAX_HEIGHT - gutter;
    }
    if (top < gutter) top = gutter;
    descAnchor = { x: left, y: top, description };
  }
</script>

<div class="apple-manager tools-manager">
  <div class="apple-scroller-proxy tools-scroller">
      <div class="tools-stack">
        <!-- 内置工具 -->
        <div class="settings-section tools-section builtin-section">
          <button
            type="button"
            class="builtin-summary"
            onclick={() => { builtinExpanded = !builtinExpanded; }}
            aria-expanded={builtinExpanded}
          >
            <div class="header-title-group" style="display: flex; align-items: baseline; gap: 10px;">
              <div class="settings-section-title" style="margin-bottom: 0;">{i18n.t('settings.tools.builtinTools')}</div>
              <span class="builtin-count-tag">{i18n.t('settings.tools.builtinCount', { ready: builtinReadyCount, total: builtinTools.length })}</span>
            </div>
            <span class="builtin-expand-icon" class:expanded={builtinExpanded}>
              <Icon name="chevronDown" size={14} />
            </span>
          </button>
          {#if builtinExpanded}
            <div class="tools-fixed-panel tools-fixed-panel--builtin" style="margin-top: 12px;">
              {#if builtinTools.length === 0}
                <div class="empty-state">
                  <Icon name="tools" size={48} />
                  <p>{i18n.t('settings.tools.noBuiltinTools')}</p>
                </div>
              {:else}
                <div class="builtin-tool-list">
                  {#each builtinTools as tool (tool.name)}
                    <div class="builtin-tool-row">
                      <div class="brand-group">
                        <div class="avatar-squircle" style="background: rgba(var(--primary-rgb, 0, 122, 255), 0.12); color: var(--primary);">
                          <Icon name={tool.name.includes('process') || tool.name.includes('shell') ? 'terminal' : 'tools'} size={13} />
                        </div>
                        <div class="identity-stack">
                          <span class="main-label">{getBuiltinToolLabel(tool.name)}</span>
                          <span class="tool-code">{tool.name}</span>
                        </div>
                      </div>
                      <div class="builtin-tool-badges">
                        <span class="tool-badge">{getBuiltinToolAccessLabel(tool.accessMode)}</span>
                        <span class="tool-badge" class:tool-badge--risk={tool.riskLevel.toLowerCase() === 'high'}>{getBuiltinToolRiskLabel(tool.riskLevel)}</span>
                        <span class="tool-runtime-status">
                          {getBuiltinToolRuntimeLabel(tool.runtimeStatus)}
                        </span>
                        <span
                          class={`apple-indicator ${getBuiltinToolRuntimeClass(tool.runtimeStatus)}`}
                          title={getBuiltinToolRuntimeTitle(tool)}
                        ></span>
                      </div>
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          {/if}
        </div>

        <!-- MCP 工具 -->
        <div class="settings-section tools-section">
          <div class="settings-section-header" style="display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 16px;">
            <div class="header-title-group" style="display: flex; align-items: baseline; gap: 10px;">
              <div class="settings-section-title" style="margin-bottom: 0;">{i18n.t('settings.tools.mcpTools')}</div>
              <div class="settings-section-desc" style="margin-bottom: 0;">{i18n.t('settings.tools.mcpDesc')}</div>
            </div>
            <div class="settings-section-actions">
              <button class="apple-action-btn" onclick={() => openMCPDialog(null)}>
                <Icon name="plus" size={14} />
                <span>{i18n.t('settings.tools.addServer')}</span>
              </button>
            </div>
          </div>
          <div class="tools-fixed-panel tools-fixed-panel--mcp">
            {#if mcpServersLoading && !mcpServersHydrated}
              <div class="empty-state">
                <Icon name="loader" size={48} />
                <p>{i18n.t('settings.tools.loading')}</p>
                <p class="empty-state-hint">{i18n.t('settings.tools.mcpLoadingHint')}</p>
              </div>
            {:else if mcpServers.length === 0}
              <div class="empty-state">
                <Icon name="tools" size={48} />
                <p>{i18n.t('settings.tools.noMcpServer')}</p>
                <p class="empty-state-hint">{i18n.t('settings.tools.noMcpServerHint')}</p>
              </div>
            {:else}
              <div class="apple-grid">
              {#each mcpServers as server (server.id)}
                <div class="apple-tile mcp-server-item" class:expanded={mcpExpandedServer === server.id} style="position: relative;">
                  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                  <div class="tile-row tile-header" role="button" tabindex="0" onclick={() => toggleMCPExpand(server.id)} onkeydown={(e) => e.key === 'Enter' && toggleMCPExpand(server.id)} style="cursor: pointer;">
                    <div class="brand-group">
                      <div class="avatar-squircle" style="background: rgba(var(--primary-rgb, 0, 122, 255), 0.15); color: var(--primary);">
                        <Icon name="plug" size={13} />
                      </div>
                      <div class="identity-stack">
                        <span class="main-label">{server.name}</span>
                        <span style="font-size: 10px; color: var(--foreground-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 140px;">{server.command || ''}</span>
                      </div>
                    </div>
                    <div class="header-action" style="display: flex; align-items: center; gap: 6px;">
                      <span class="apple-indicator" class:success={server.health === 'connected'} class:warning={server.health === 'degraded'} class:error={server.health === 'disconnected' || !server.health} title={server.error || getMCPHealthLabel(server)}></span>
                      <span class="mcp-expand-icon" class:expanded={mcpExpandedServer === server.id} style="margin-left: 4px;">
                        <Icon name="chevronDown" size={14} />
                      </span>
                    </div>
                  </div>
                  
                  <div class="tile-row tile-body" style="height: 32px; display: flex; align-items: flex-start; margin-top: 4px;">
                    <p class="apple-summary" title={server.error || getMCPHealthLabel(server)} style="margin: 0; font-size: 11px; color: {server.error ? 'var(--error)' : 'var(--foreground-muted)'}; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;">
                      {i18n.t('settings.tools.runtimeStatus', { status: getMCPHealthLabel(server) })}
                      {#if server.error} - {server.error}{/if}
                    </p>
                  </div>

                  <div class="tile-row tile-footer" style="margin-top: auto;">
                    <div class="footer-left">
                      <Toggle
                        checked={server.enabled}
                        title={server.enabled ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
                        onchange={() => toggleMCPServer(server.id, server.enabled)}
                      />
                    </div>
                    <div class="footer-right" style="display: flex; gap: 4px;">
                      <button class="btn-icon btn-icon--sm" title={i18n.t('settings.tools.edit')} onclick={(e) => { e.stopPropagation(); openMCPDialog(server); }}>
                        <Icon name="edit" size={14} />
                      </button>
                      <button class="btn-icon btn-icon--sm btn-icon--danger" title={i18n.t('settings.tools.delete')} onclick={(e) => { e.stopPropagation(); deleteMCPServer(server.id); }}>
                        <Icon name="trash" size={14} />
                      </button>
                    </div>
                  </div>

                  {#if mcpExpandedServer === server.id}
                    <div class="mcp-tools-popover">
                      <div class="mcp-tools-header">
                        <span>{i18n.t('settings.tools.toolList')} {mcpServerTools[server.id]?.length ? `(${mcpServerTools[server.id].length})` : ''}</span>
                        <button class="btn-icon btn-icon--sm" class:refreshing={mcpRefreshingServers.has(server.id)} title={i18n.t('settings.tools.refreshTools')}
                          onclick={() => refreshMCPTools(server.id)} disabled={mcpRefreshingServers.has(server.id)}>
                          <Icon name="refresh" size={12} />
                        </button>
                      </div>
                      <div class="mcp-tools-list" style="display: flex; flex-direction: column; gap: 6px;">
                        {#if mcpRefreshingServers.has(server.id)}
                          <div class="mcp-tools-empty" style="font-size: 11px; padding: 12px; text-align: center;">{i18n.t('settings.tools.loading')}</div>
                        {:else if mcpServerTools[server.id] && mcpServerTools[server.id].length > 0}
                          {#each mcpServerTools[server.id] as tool}
                            <div class="mcp-tool-item" style="background: rgba(var(--foreground-rgb), 0.04); border-radius: 6px; padding: 6px 8px;">
                              <div class="mcp-tool-row" style="display: flex; justify-content: space-between; align-items: center;">
                                <div class="mcp-tool-name" style="font-size: 11px; font-weight: 500;">{tool.name}</div>
                                {#if tool.description}
                                  <button
                                    class="mcp-tool-desc-btn"
                                    title={i18n.t('settings.tools.viewDesc')}
                                    onmouseenter={(e) => openDesc(tool.description, e)}
                                    onmouseleave={scheduleClose}
                                    onfocus={(e) => openDesc(tool.description, e as unknown as MouseEvent)}
                                    onblur={scheduleClose}
                                    style="transform: scale(0.8);"
                                  >
                                    <Icon name="info" size={12} />
                                  </button>
                                {/if}
                              </div>
                            </div>
                          {/each}
                        {:else}
                          <div class="mcp-tools-empty" style="font-size: 11px; padding: 12px; text-align: center;">{i18n.t('settings.tools.noToolsHint')}</div>
                        {/if}
                      </div>
                    </div>
                  {/if}
                </div>
              {/each}
              </div>
            {/if}
          </div>
        </div>

        <!-- Claude Skills 工具 -->
        <div class="settings-section tools-section">
          <div class="settings-section-header" style="display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 16px;">
            <div class="header-title-group" style="display: flex; align-items: baseline; gap: 10px;">
              <div class="settings-section-title" style="margin-bottom: 0;">{i18n.t('settings.tools.claudeSkills')}</div>
              <div class="settings-section-desc" style="margin-bottom: 0;">{i18n.t('settings.tools.skillsDesc')}</div>
            </div>
            <div class="settings-section-actions">
              <button class="apple-action-btn" onclick={() => openSkillLibraryDialog()}>
                <Icon name="plus" size={14} />
                <span>{i18n.t('settings.tools.installSkill')}</span>
              </button>
              <button class="apple-action-btn" onclick={() => openRepoDialog()}>
                <Icon name="grid" size={14} />
                <span>{i18n.t('settings.tools.manageRepos')}</span>
              </button>
            </div>
          </div>
          <div class="tools-fixed-panel tools-fixed-panel--skills">
            {#if skills.length === 0}
              <div class="empty-state">
                <Icon name="tools" size={48} />
                <p>{i18n.t('settings.tools.noSkills')}</p>
                <p class="empty-state-hint">{i18n.t('settings.tools.noSkillsHint')}</p>
              </div>
            {:else}
              <div class="apple-grid">
              {#each skills as skill}
                <div class="apple-tile skill-item">
                  <div class="skill-head">
                    <div class="skill-brand">
                      <div class="skill-avatar">
                        <Icon name="tools" size={12} />
                      </div>
                      <div class="skill-name-box">
                        <span class="skill-name">{skill.name}</span>
                        <span class="skill-source-tag">{skill.source === 'custom' ? i18n.t('settings.tools.custom') : 'Instruction'}</span>
                      </div>
                    </div>
                    <button class="skill-delete-btn" title={i18n.t('settings.tools.delete')} onclick={() => deleteSkill(skill)}>
                      <Icon name="trash" size={12} />
                    </button>
                  </div>
                  {#if skill.description}
                    <div class="skill-body">
                      <p class="skill-desc" title={skill.description}>{skill.description}</p>
                    </div>
                  {/if}
                </div>
              {/each}
              </div>
            {/if}
          </div>
        </div>
      </div>
    </div>
</div>

{#if descAnchor}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="mcp-tool-desc-popover"
    style="top: {descAnchor.y}px; left: {descAnchor.x}px;"
    role="tooltip"
    onmouseenter={clearCloseTimer}
    onmouseleave={scheduleClose}
  >
    {descAnchor.description}
  </div>
{/if}

<style>
  .tools-manager {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .tools-scroller {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 0;
    overflow-y: auto;
  }

  .tools-stack {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    padding: 0 4px 12px 4px;
    box-sizing: border-box;
  }

  .tools-section {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 0 0 auto;
  }

  .tools-fixed-panel {
    min-height: 0;
    overflow: visible;
    display: flex;
    flex-direction: column;
  }

  .tools-fixed-panel--builtin {
    flex: 0 0 auto;
    max-height: 318px;
    overflow-y: auto;
    overflow-x: hidden;
    padding-right: 2px;
    overscroll-behavior: contain;
    scrollbar-width: thin;
    scrollbar-color: var(--scrollbar-thumb) transparent;
  }

  .tools-fixed-panel--builtin::-webkit-scrollbar {
    width: 10px;
  }

  .tools-fixed-panel--builtin::-webkit-scrollbar-track {
    background: color-mix(in srgb, var(--surface-2) 58%, transparent);
    border-radius: 999px;
  }

  .tools-fixed-panel--builtin::-webkit-scrollbar-thumb {
    background: var(--scrollbar-thumb);
    border-radius: 999px;
    border: 2px solid color-mix(in srgb, var(--surface-1) 88%, transparent);
    background-clip: content-box;
  }

  .tools-fixed-panel > .empty-state {
    flex: 1 1 auto;
    min-height: 0;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .tools-fixed-panel .apple-grid {
    min-height: 0;
  }

  .builtin-section {
    margin-bottom: 16px;
  }

  .builtin-summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 8px 12px;
    background: rgba(var(--foreground-rgb), 0.025);
    border: 1px solid var(--border);
    border-radius: 10px;
    cursor: pointer;
    transition: background 0.15s ease, border-color 0.15s ease;
  }

  .builtin-summary:hover {
    background: rgba(var(--foreground-rgb), 0.05);
    border-color: rgba(var(--primary-rgb, 0, 122, 255), 0.35);
  }

  .builtin-count-tag {
    font-size: 11px;
    color: var(--foreground-muted);
    padding: 2px 8px;
    border-radius: 999px;
    background: rgba(var(--foreground-rgb), 0.06);
  }

  .builtin-expand-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: transform 0.2s cubic-bezier(0.3, 0, 0.2, 1);
    color: var(--foreground-muted);
  }

  .builtin-expand-icon.expanded {
    transform: rotate(180deg);
  }

  .builtin-tool-list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 8px;
  }

  .builtin-tool-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    min-width: 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: rgba(var(--foreground-rgb), 0.035);
  }

  .tool-code {
    font-size: 10px;
    color: var(--foreground-muted);
    font-family: var(--font-mono);
  }

  .builtin-tool-badges {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .tool-badge {
    padding: 2px 6px;
    border-radius: 999px;
    background: rgba(var(--foreground-rgb), 0.06);
    color: var(--foreground-muted);
    font-size: 10px;
    font-weight: 500;
    white-space: nowrap;
  }

  .tool-badge--risk {
    color: var(--warning, #b7791f);
    background: rgba(181, 118, 20, 0.12);
  }

  .tool-runtime-status {
    font-size: 10px;
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .mcp-tools-popover {
    position: absolute;
    top: calc(100% + 8px);
    left: 0;
    width: 100%;
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: 0 18px 40px rgba(0, 0, 0, 0.35), 0 1px 2px rgba(0, 0, 0, 0.08);
    z-index: var(--z-popover);
    padding: 12px;
    max-height: 250px;
    overflow-y: auto;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
  }

  .mcp-tools-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 12px;
    font-weight: 500;
    color: var(--foreground-muted);
    margin-bottom: 12px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border);
  }

  /* Apply consistent tile classes */
  .apple-tile {
    background: rgba(255, 255, 255, 0.92);
    border: 1px solid rgba(60, 60, 67, 0.16);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04), 0 6px 18px rgba(0, 0, 0, 0.05);
    border-radius: 12px;
    padding: 14px 18px 18px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    height: 124px;
    box-sizing: border-box;
    transition: background 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease;
    position: relative;
    z-index: 1;
  }

  .apple-tile:hover {
    border-color: rgba(60, 60, 67, 0.2);
    background: #ffffff;
    z-index: 5;
  }

  :global(body.theme-dark) .apple-tile,
  :global(body.vscode-dark) .apple-tile,
  :global(:root.theme-dark) .apple-tile {
    background: rgba(255, 255, 255, 0.04);
    border-color: rgba(255, 255, 255, 0.14);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04), 0 6px 18px rgba(0, 0, 0, 0.05);
  }

  :global(body.theme-dark) .apple-tile:hover,
  :global(body.vscode-dark) .apple-tile:hover,
  :global(:root.theme-dark) .apple-tile:hover {
    border-color: rgba(255, 255, 255, 0.20);
    background: rgba(255, 255, 255, 0.07);
  }

  .apple-tile.expanded {
    z-index: 100;
  }

  .brand-group {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }

  .avatar-squircle {
    width: 24px;
    height: 24px;
    border-radius: 7px;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .identity-stack {
    display: flex;
    flex-direction: column;
    justify-content: center;
    min-width: 0;
  }

  .main-label {
    font-size: 13.5px;
    font-weight: 600;
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tile-row {
    display: flex;
    align-items: center;
    width: 100%;
  }

  .tile-header {
    justify-content: space-between;
    height: 24px;
    flex-shrink: 0;
  }

  .mcp-expand-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: transform 0.25s cubic-bezier(0.3, 0, 0.2, 1);
  }

  .mcp-expand-icon.expanded {
    transform: rotate(180deg);
  }

  .mcp-tools-list { display: flex; flex-direction: column; gap: var(--space-2); }

  /* MCP 工具项样式 */
  .mcp-tool-item {
    position: relative;
    padding: var(--space-2) var(--space-3);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }
  .mcp-tool-item:hover { border-color: var(--primary-muted); background: var(--surface-hover); }
  .mcp-tool-row { display: flex; align-items: center; gap: var(--space-2); }
  .mcp-tool-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

  /* MCP 工具描述浮层气泡：fixed 定位，已在脚本中按视口边界 clamp */
  .mcp-tool-desc-popover {
    position: fixed;
    width: 320px;
    max-height: 240px;
    overflow-y: auto;
    padding: 10px 12px;
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 18px 40px rgba(0, 0, 0, 0.35), 0 1px 2px rgba(0, 0, 0, 0.08);
    font-size: var(--text-xs);
    line-height: 1.5;
    color: var(--foreground);
    z-index: calc(var(--z-popover) + 10);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    box-sizing: border-box;
  }

  /* MCP 工具描述查看按钮 */
  .mcp-tool-desc-btn {
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--foreground-muted);
    transition: all var(--transition-fast);
    flex-shrink: 0;
  }
  .mcp-tool-desc-btn:hover {
    background: var(--surface-hover);
    border-color: var(--primary);
    color: var(--foreground);
  }

  .mcp-tools-empty { font-size: var(--text-sm); color: var(--foreground-muted); text-align: center; padding: var(--space-4); }

  /* Skill 卡片内部 */
  .skill-item { min-height: auto; height: auto; align-self: start; }
  .skill-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .skill-brand { display: flex; align-items: center; gap: 8px; min-width: 0; flex: 1; }
  .skill-avatar { width: 22px; height: 22px; border-radius: 6px; display: flex; align-items: center; justify-content: center; flex-shrink: 0; background: rgba(var(--success-rgb, 52, 199, 89), 0.12); color: var(--success); }
  .skill-name-box { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .skill-name { font-size: 13px; font-weight: 650; color: var(--foreground); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; letter-spacing: -0.01em; }
  .skill-source-tag { font-size: 8px; font-weight: 600; padding: 1px 5px; border-radius: 5px; background: transparent; border: 1px solid rgba(var(--foreground-rgb), 0.12); color: var(--foreground-muted); letter-spacing: 0.04em; white-space: nowrap; flex-shrink: 0; }
  .skill-delete-btn { width: 24px; height: 24px; border-radius: 6px; border: none; background: transparent; color: var(--foreground-muted); cursor: pointer; display: flex; align-items: center; justify-content: center; flex-shrink: 0; opacity: 0; transition: opacity 0.18s ease, color 0.18s ease, background 0.18s ease; }
  .skill-item:hover .skill-delete-btn { opacity: 1; }
  .skill-delete-btn:hover { color: var(--error); background: rgba(var(--error-rgb, 255, 59, 48), 0.1); }
  .skill-body { margin-top: 2px; }
  .skill-desc { margin: 0; font-size: 11px; color: var(--foreground-muted); line-height: 1.45; display: -webkit-box; line-clamp: 2; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }

  /* 空状态 */
  
  
  
</style>

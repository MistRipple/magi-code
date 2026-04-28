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
    mcpExpandedTool,
    builtinTools,
    skills,
    openMCPDialog,
    toggleMCPExpand,
    getMCPHealthLabel,
    toggleMCPServer,
    deleteMCPServer,
    refreshMCPTools,
    toggleMCPToolDesc,
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
    mcpExpandedTool: string | null;
    builtinTools: Array<{
      name: string;
      riskLevel: string;
      approvalRequirement: string;
      accessMode: string;
      enabled: boolean;
    }>;
    skills: any[];
    openMCPDialog: (server: any) => void;
    toggleMCPExpand: (sid: string) => void;
    getMCPHealthLabel: (server: any) => string;
    toggleMCPServer: (sid: string, enabled: boolean) => void;
    deleteMCPServer: (sid: string) => void;
    refreshMCPTools: (sid: string) => void;
    toggleMCPToolDesc: (toolKey: string, e: MouseEvent) => void;
    openSkillLibraryDialog: () => void;
    openRepoDialog: () => void;
    deleteSkill: (skill: any) => void;
  }>();

  const BUILTIN_TOOL_LABELS: Record<string, string> = {
    shell_exec: 'Shell 命令',
    process_launch: '长任务启动',
    process_read: '进程输出读取',
    process_write: '进程输入写入',
    process_kill: '进程终止',
    process_list: '进程列表',
    process_inspect: '进程检查',
    file_read: '读取文件',
    file_write: '写入文件',
    file_patch: '修改文件',
    file_remove: '删除文件',
    file_mkdir: '创建目录',
    file_copy: '复制文件',
    file_move: '移动文件',
    search_text: '文本搜索',
    search_semantic: '语义搜索',
    diff_preview: '差异预览',
    web_search: '网页搜索',
    web_fetch: '网页读取',
    mermaid_diagram: 'Mermaid 图表',
    knowledge_query: '知识库检索',
  };

  function getBuiltinToolLabel(name: string): string {
    return BUILTIN_TOOL_LABELS[name] ?? name;
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
</script>

<div class="apple-manager" style="flex: 1; min-height: 0; display: flex; flex-direction: column;">
  <div class="apple-scroller-proxy" style="flex: 1; display: flex; flex-direction: column; padding: 0;">
      <div style="display: flex; flex-direction: column; min-height: 100%; flex: 1; padding: 0 4px 12px 4px; box-sizing: border-box;">
        <!-- 内置工具 -->
        <div class="settings-section tools-section">
          <div class="settings-section-header" style="display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 16px;">
            <div class="header-title-group" style="display: flex; align-items: baseline; gap: 10px;">
              <div class="settings-section-title" style="margin-bottom: 0;">{i18n.t('settings.tools.builtinTools')}</div>
              <div class="settings-section-desc" style="margin-bottom: 0;">{i18n.t('settings.tools.builtinDesc')}</div>
            </div>
          </div>
          <div class="tools-fixed-panel tools-fixed-panel--builtin">
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
                      <span class="apple-indicator" class:success={tool.enabled} title={tool.enabled ? i18n.t('settings.tools.enabled') : i18n.t('settings.tools.disabledLabel')}></span>
                    </div>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
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
                          {#each mcpServerTools[server.id] as tool, toolIndex}
                            {@const toolKey = `${server.id}-${toolIndex}`}
                            <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
                            <div class="mcp-tool-item" class:show-desc={mcpExpandedTool === toolKey} style="background: rgba(var(--foreground-rgb), 0.04); border-radius: 6px; padding: 6px 8px;">
                              <div class="mcp-tool-row" style="display: flex; justify-content: space-between; align-items: center;">
                                <div class="mcp-tool-name" style="font-size: 11px; font-weight: 500;">{tool.name}</div>
                                {#if tool.description}
                                  <button class="mcp-tool-desc-btn" title={i18n.t('settings.tools.viewDesc')} onclick={(e) => toggleMCPToolDesc(toolKey, e)} style="transform: scale(0.8);">
                                    <Icon name="info" size={12} />
                                  </button>
                                {/if}
                              </div>
                              {#if tool.description && mcpExpandedTool === toolKey}
                                <div class="mcp-tool-desc" style="font-size: 10px; margin-top: 4px; color: var(--foreground-muted); line-height: 1.4;">{tool.description}</div>
                              {/if}
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

<style>
  .tools-section {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  .tools-fixed-panel {
    min-height: 0;
    overflow: visible;
    display: flex;
    flex-direction: column;
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

  .mcp-tools-popover {
    position: absolute;
    top: calc(100% + 8px);
    left: 0;
    width: 100%;
    background: rgba(255, 255, 255, 0.96);
    backdrop-filter: blur(24px);
    -webkit-backdrop-filter: blur(24px);
    border: 1px solid rgba(60, 60, 67, 0.16);
    border-radius: 12px;
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.12), 0 1px 2px rgba(0, 0, 0, 0.04);
    z-index: 1000;
    padding: 12px;
    max-height: 250px;
    overflow-y: auto;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
  }

  :global(body.theme-dark) .mcp-tools-popover,
  :global(body.vscode-dark) .mcp-tools-popover,
  :global(:root.theme-dark) .mcp-tools-popover {
    background: rgba(255, 255, 255, 0.06);
    border-color: rgba(255, 255, 255, 0.14);
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.3), 0 1px 2px rgba(0, 0, 0, 0.1);
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
    border-bottom: 1px solid rgba(60, 60, 67, 0.10);
  }

  :global(body.theme-dark) .mcp-tools-header,
  :global(body.vscode-dark) .mcp-tools-header,
  :global(:root.theme-dark) .mcp-tools-header {
    border-bottom-color: rgba(255, 255, 255, 0.08);
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

  .mcp-tools-header { display: flex; justify-content: space-between; align-items: center; font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground-muted); margin-bottom: var(--space-2); }
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
  .mcp-tool-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: 2px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }

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

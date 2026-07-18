<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import type { IconName } from '../lib/icons';
  import {
    getBuiltinToolFallbackLabel,
    getCapabilityDependencyFallbackLabel,
    summarizeMcpServers,
  } from '../shared/tool-display';

  let {
    mcpServers,
    mcpServersHydrated,
    mcpServersLoading,
    mcpExpandedServer,
    mcpServerTools,
    mcpRefreshingServers,
    builtinTools,
    builtinToolsLoading,
    capabilityDependencies,
    commandEnvironment,
    commandEnvironmentLoading,
    skills,
    skillUpdateAvailableCount,
    skillUpdatesChecking,
    skillUpdatingIds,
    skillTogglingIds,
    openMCPDialog,
    toggleMCPExpand,
    getMCPHealthLabel,
    toggleMCPServer,
    deleteMCPServer,
    refreshMCPTools,
    refreshBuiltinToolCatalog,
    refreshCommandEnvironment,
    openSkillLibraryDialog,
    openRepoDialog,
    checkSkillUpdates,
    updateSkill,
    toggleSkill,
    updateAllSkills,
    rollbackSkill,
    deleteSkill
  } = $props<{
    mcpServers: any[];
    mcpServersHydrated: boolean;
    mcpServersLoading: boolean;
    mcpExpandedServer: string | null;
    mcpServerTools: Record<string, any[]>;
    mcpRefreshingServers: Set<string>;
    builtinToolsLoading: boolean;
    builtinTools: Array<{
      name: string;
      riskLevel: string;
      approvalRequirement: string;
      effectiveApprovalPolicy: string;
      accessProfileBehavior: string;
      accessMode: string;
      policyScope: string;
      inputSensitivePolicy: boolean;
      policySummary: string;
      runtimeInternal: boolean;
      runtimeStatus: string;
      runtimeWarnings: string[];
      schemaStatus: string;
      schemaWarnings: string[];
      enabled: boolean;
    }>;
    capabilityDependencies: Array<{
      name: string;
      status: string;
      requiredBy: string[];
      roleCount?: number | null;
      spawnableRoleCount?: number | null;
      configuredCount?: number | null;
      enabledCount?: number | null;
      readyCount?: number | null;
      toolCount?: number | null;
    }>;
    commandEnvironment: {
      source: string;
      pathAvailable: boolean;
      commands: Array<{ name: string; available: boolean; path: string | null }>;
    } | null;
    commandEnvironmentLoading: boolean;
    skills: any[];
    skillUpdateAvailableCount: number;
    skillUpdatesChecking: boolean;
    skillUpdatingIds: Set<string>;
    skillTogglingIds: Set<string>;
    openMCPDialog: (server: any) => void;
    toggleMCPExpand: (sid: string) => void;
    getMCPHealthLabel: (server: any) => string;
    toggleMCPServer: (sid: string, enabled: boolean) => void;
    deleteMCPServer: (sid: string) => void;
    refreshMCPTools: (sid: string) => void;
    refreshBuiltinToolCatalog: () => void | Promise<void>;
    refreshCommandEnvironment: () => void | Promise<void>;
    openSkillLibraryDialog: () => void;
    openRepoDialog: () => void;
    checkSkillUpdates: () => void | Promise<void>;
    updateSkill: (skillId: string) => void | Promise<void>;
    toggleSkill: (skillId: string, enabled: boolean) => void | Promise<void>;
    updateAllSkills: () => void | Promise<void>;
    rollbackSkill: (skillId: string) => void | Promise<void>;
    deleteSkill: (skill: any) => void;
  }>();

  function getSkillSourceLabel(skill: any): string {
    if (skill.origin === 'repository') return i18n.t('settings.tools.skillSourceRepository');
    if (skill.origin === 'local') return i18n.t('settings.tools.skillSourceLocal');
    return i18n.t('settings.tools.custom');
  }

  function getSkillStatusLabel(skill: any): string {
    switch (skill.updateStatus) {
      case 'update_available':
        return i18n.t('settings.tools.skillStatusUpdateAvailable');
      case 'local_changed':
        return i18n.t('settings.tools.skillStatusLocalChanged');
      case 'local_modified':
        return i18n.t('settings.tools.skillStatusLocalModified');
      case 'source_missing':
        return i18n.t('settings.tools.skillStatusSourceMissing');
      case 'source_removed':
        return i18n.t('settings.tools.skillStatusSourceRemoved');
      default:
        return i18n.t('settings.tools.skillStatusCurrent');
    }
  }

  function getSkillStatusClass(skill: any): string {
    if (skill.updateStatus === 'update_available' || skill.updateStatus === 'local_changed') return 'warning';
    if (skill.updateStatus === 'source_missing' || skill.updateStatus === 'source_removed') return 'error';
    if (skill.updateStatus === 'local_modified') return 'modified';
    return 'success';
  }

  function formatSkillCheckedAt(value: number): string {
    return new Intl.DateTimeFormat(i18n.locale, {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    }).format(new Date(value));
  }

  function builtinToolI18nKey(name: string): string {
    const suffix = name
      .split('_')
      .filter(Boolean)
      .map((part, index) => index === 0 ? part : part.charAt(0).toUpperCase() + part.slice(1))
      .join('');
    return suffix ? `settings.tools.builtin.${suffix}` : '';
  }

  function formatBuiltinToolFallbackLabel(name: string): string {
    return getBuiltinToolFallbackLabel(name);
  }

  function getBuiltinToolLabel(name: string): string {
    const key = builtinToolI18nKey(name);
    const translated = key ? i18n.t(key) : '';
    if (translated && translated !== key) return translated;
    return i18n.locale === 'zh-CN'
      ? i18n.t('settings.tools.builtin.unknown')
      : formatBuiltinToolFallbackLabel(name);
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
        return i18n.t('settings.tools.riskUnspecified');
    }
  }

  function getBuiltinToolRuntimeLabel(status: string): string {
    switch (status) {
      case 'ready':
        return i18n.t('settings.tools.runtimeReady');
      case 'degraded':
        return i18n.t('settings.tools.runtimeDegraded');
      case 'not_ready':
        return i18n.t('settings.tools.runtimeNotReady');
      case 'unavailable':
        return i18n.t('settings.tools.runtimeUnavailable');
      default:
        return i18n.t('settings.tools.runtimeChecking');
    }
  }

  function getBuiltinToolRuntimeClass(status: string): string {
    switch (status) {
      case 'ready':
        return 'success';
      case 'degraded':
      case 'not_ready':
        return 'warning';
      case 'unavailable':
        return 'error';
      default:
        return 'disabled';
    }
  }

  function getBuiltinToolDisplayStatusLabel(tool: { runtimeStatus: string; accessProfileBehavior: string }): string {
    if (tool.accessProfileBehavior === 'unavailable_in_read_only') {
      return getBuiltinToolAccessProfileBehaviorLabel(tool.accessProfileBehavior);
    }
    return getBuiltinToolRuntimeLabel(tool.runtimeStatus);
  }

  function getBuiltinToolDisplayStatusClass(tool: { runtimeStatus: string; accessProfileBehavior: string }): string {
    if (tool.accessProfileBehavior === 'unavailable_in_read_only') {
      return 'disabled';
    }
    return getBuiltinToolRuntimeClass(tool.runtimeStatus);
  }

  function getBuiltinToolRuntimeTitle(tool: {
    runtimeInternal: boolean;
    runtimeStatus: string;
    runtimeWarnings: string[];
    schemaWarnings: string[];
    effectiveApprovalPolicy: string;
    accessProfileBehavior: string;
  }): string {
    const parts = [getBuiltinToolRuntimeLabel(tool.runtimeStatus)];
    parts.push(i18n.t('settings.tools.effectiveApprovalPolicy', {
      policy: getBuiltinToolEffectiveApprovalLabel(tool.effectiveApprovalPolicy),
    }));
    parts.push(getBuiltinToolAccessProfileBehaviorLabel(tool.accessProfileBehavior));
    if (tool.runtimeWarnings.length > 0) parts.push(i18n.t('settings.tools.runtimeWarning'));
    if (tool.schemaWarnings.length > 0) parts.push(i18n.t('settings.tools.schemaWarning'));
    return parts.join('\n');
  }

  function getBuiltinToolEffectiveApprovalLabel(policy: string): string {
    switch (policy) {
      case 'input_sensitive':
        return i18n.t('settings.tools.effectiveApproval.inputSensitive');
      case 'required':
        return i18n.t('settings.tools.effectiveApproval.required');
      case 'regular_risk_block_skipped':
        return i18n.t('settings.tools.effectiveApproval.ordinaryApprovalSkipped');
      case 'not_applicable':
        return i18n.t('settings.tools.effectiveApproval.notApplicable');
      case 'none':
        return i18n.t('settings.tools.effectiveApproval.none');
      default:
        return i18n.t('settings.tools.effectiveApproval.unspecified');
    }
  }

  function getBuiltinToolAccessProfileBehaviorLabel(behavior: string): string {
    switch (behavior) {
      case 'unavailable_in_read_only':
        return i18n.t('settings.tools.accessProfileBehavior.unavailableInReadOnly');
      case 'read_only_allowed':
        return i18n.t('settings.tools.accessProfileBehavior.readOnlyAllowed');
      case 'restricted_input_sensitive':
        return i18n.t('settings.tools.accessProfileBehavior.restrictedInputSensitive');
      case 'restricted_blocks_high_risk':
        return i18n.t('settings.tools.accessProfileBehavior.restrictedRequiresApproval');
      case 'restricted_allowed':
        return i18n.t('settings.tools.accessProfileBehavior.restrictedAllowed');
      case 'full_access_skips_regular_risk_blocks':
        return i18n.t('settings.tools.accessProfileBehavior.fullAccessSkipsOrdinaryApproval');
      case 'full_access_allowed':
        return i18n.t('settings.tools.accessProfileBehavior.fullAccessAllowed');
      default:
        return i18n.t('settings.tools.accessProfileBehavior.undetermined');
    }
  }

  function getBuiltinToolScopeLabel(tool: { runtimeInternal: boolean }): string {
    return i18n.t(tool.runtimeInternal
      ? 'settings.tools.builtin.runtimeCapability'
      : 'settings.tools.builtin.localCapability');
  }

  function getMCPServerSubtitle(server: any): string {
    if (server?.enabled === false) {
      return i18n.t('settings.tools.disabledLabel');
    }
    if (server?.health !== 'connected') {
      return getMCPHealthLabel(server);
    }
    if (typeof server?.toolCount === 'number' && Number.isFinite(server.toolCount) && server.toolCount > 0) {
      return i18n.t('settings.tools.mcpToolCount', { count: server.toolCount });
    }
    return getMCPHealthLabel(server);
  }

  function getCapabilityDependencyLabel(name: string): string {
    switch (name) {
      case 'knowledge_store':
        return i18n.t('settings.tools.dependency.knowledgeStore');
      case 'workspace_code_index':
        return i18n.t('settings.tools.dependency.workspaceCodeIndex');
      case 'agent_role_registry':
        return i18n.t('settings.tools.dependency.agentRoleRegistry');
      case 'file_snapshot':
        return i18n.t('settings.tools.dependency.fileSnapshot');
      case 'context_runtime':
        return i18n.t('settings.tools.dependency.contextRuntime');
      case 'skill_runtime':
        return i18n.t('settings.tools.dependency.skillRuntime');
      case 'mcp_servers':
        return i18n.t('settings.tools.dependency.mcpServers');
      case 'image_generation_model':
        return i18n.t('settings.tools.dependency.imageGenerationModel');
      default:
        return getCapabilityDependencyFallbackLabel(name);
    }
  }

  function getCapabilityDependencyStatusLabel(status: string): string {
    switch (status) {
      case 'available':
        return i18n.t('settings.tools.dependency.status.available');
      case 'ready':
        return i18n.t('settings.tools.dependency.status.ready');
      case 'degraded':
        return i18n.t('settings.tools.dependency.status.degraded');
      case 'not_ready':
        return i18n.t('settings.tools.dependency.status.notReady');
      case 'unavailable':
        return i18n.t('settings.tools.dependency.status.unavailable');
      default:
        return i18n.t('settings.tools.dependency.status.checking');
    }
  }

  function getCapabilityDependencyClass(status: string): string {
    switch (status) {
      case 'available':
      case 'ready':
        return 'success';
      case 'degraded':
      case 'not_ready':
        return 'warning';
      case 'unavailable':
        return 'error';
      case 'checking':
        return 'warning';
      default:
        return 'warning';
    }
  }

  function getCapabilityDependencyIcon(name: string): IconName {
    switch (name) {
      case 'knowledge_store':
        return 'database';
      case 'workspace_code_index':
        return 'search';
      case 'agent_role_registry':
        return 'bot';
      case 'file_snapshot':
        return 'file';
      case 'context_runtime':
        return 'brain';
      case 'skill_runtime':
        return 'skill';
      case 'mcp_servers':
        return 'plug';
      case 'image_generation_model':
        return 'sparkles';
      default:
        return 'tools';
    }
  }

  function getCapabilityDependencyMetric(dependency: {
    name: string;
    spawnableRoleCount?: number | null;
    configuredCount?: number | null;
    enabledCount?: number | null;
    readyCount?: number | null;
    enabledToolCount?: number | null;
    readyToolCount?: number | null;
    toolCount?: number | null;
  }): string {
    if (dependency.name === 'agent_role_registry' && typeof dependency.spawnableRoleCount === 'number') {
      return i18n.t('settings.tools.dependency.spawnableRoleCount', {
        count: dependency.spawnableRoleCount,
      });
    }
    if (
      dependency.name === 'skill_runtime'
      && typeof dependency.configuredCount === 'number'
      && typeof dependency.toolCount === 'number'
    ) {
      return i18n.t('settings.tools.dependency.skillCount', {
        skills: dependency.configuredCount,
        tools: dependency.toolCount,
      });
    }
    if (
      dependency.name === 'mcp_servers'
      && typeof dependency.enabledCount === 'number'
      && typeof dependency.readyCount === 'number'
    ) {
      return i18n.t('settings.tools.dependency.readyServerCount', {
        ready: dependency.readyCount,
        total: dependency.enabledCount,
      });
    }
    if (dependency.name === 'mcp_servers' && typeof dependency.configuredCount === 'number') {
      return i18n.t('settings.tools.dependency.configuredServerCount', {
        count: dependency.configuredCount,
      });
    }
    return '';
  }

  function getCapabilityDependencyConsumerLabel(name: string): string {
    switch (name) {
      case 'knowledge_query':
        return i18n.t('settings.tools.dependency.consumer.knowledgeQuery');
      case 'search_semantic':
      case 'code_symbols':
        return i18n.t('settings.tools.dependency.consumer.localCodeSearch');
      case 'agent_spawn':
      case 'agent_wait':
        return i18n.t('settings.tools.dependency.consumer.subagentTasks');
      case 'task_execution':
        return i18n.t('settings.tools.dependency.consumer.taskExecution');
      case 'conversation_context':
        return i18n.t('settings.tools.dependency.consumer.conversationContext');
      case 'knowledge_memory_selection':
        return i18n.t('settings.tools.dependency.consumer.knowledgeMemorySelection');
      case 'changes/diff':
      case 'changes/approve':
      case 'changes/revert':
        return i18n.t('settings.tools.dependency.consumer.fileChangeManagement');
      case 'skill prompt context':
        return i18n.t('settings.tools.dependency.consumer.skillContext');
      case 'skill custom tools':
        return i18n.t('settings.tools.dependency.consumer.skillTools');
      case 'mcp custom tools':
      case 'skill MCP bridge tools':
        return i18n.t('settings.tools.dependency.consumer.mcpTools');
      default:
        return i18n.t('settings.tools.dependency.consumer.other');
    }
  }

  function formatCapabilityDependencyConsumers(requiredBy: string[]): string {
    const labels = requiredBy.map(getCapabilityDependencyConsumerLabel);
    const uniqueLabels = Array.from(new Set(labels)).filter(Boolean);
    return uniqueLabels.join(i18n.t('settings.tools.dependency.consumer.separator'));
  }

  function getCapabilityDependencyTitle(dependency: {
    name: string;
    status: string;
    requiredBy: string[];
    spawnableRoleCount?: number | null;
    configuredCount?: number | null;
    enabledCount?: number | null;
    readyCount?: number | null;
    enabledToolCount?: number | null;
    readyToolCount?: number | null;
    toolCount?: number | null;
  }): string {
    const displayStatus = getCapabilityDependencyDisplayStatus(dependency);
    const parts = [`${getCapabilityDependencyLabel(dependency.name)}: ${displayStatus}`];
    const metric = getCapabilityDependencyMetric(dependency);
    if (metric) parts.push(metric);
    if (dependency.requiredBy.length > 0) {
      parts.push(i18n.t('settings.tools.dependency.requiredBy', {
        tools: formatCapabilityDependencyConsumers(dependency.requiredBy),
      }));
    }
    return parts.join('\n');
  }

  function getMCPOverviewSummary() {
    return summarizeMcpServers(mcpServers, mcpServersHydrated, mcpServersLoading);
  }

  function getMCPOverviewStatusLabel(): string {
    const summary = getMCPOverviewSummary();
    switch (summary.kind) {
      case 'checking':
        return i18n.t('settings.tools.mcpHealthChecking');
      case 'not_configured':
        return i18n.t('settings.tools.mcpHealthNotConfigured');
      case 'disabled':
        return i18n.t('settings.tools.disabledLabel');
      case 'connected':
        return i18n.t('settings.tools.mcpHealthConnected');
      case 'partial':
        return i18n.t('settings.tools.mcpHealthPartial', {
          connected: summary.connected,
          total: summary.enabled,
        });
      case 'disconnected':
        return i18n.t('settings.tools.mcpHealthDisconnected');
    }
  }

  function getCapabilityDependencyDisplayStatus(dependency: { name: string; status: string }): string {
    return dependency.name === 'mcp_servers'
      ? getMCPOverviewStatusLabel()
      : getCapabilityDependencyStatusLabel(dependency.status);
  }

  function getCapabilityDependencyDisplayClass(dependency: { name: string; status: string }): string {
    if (dependency.name !== 'mcp_servers') {
      return getCapabilityDependencyClass(dependency.status);
    }
    const summary = getMCPOverviewSummary();
    switch (summary.kind) {
      case 'disconnected':
        return 'error';
      case 'disabled':
      case 'not_configured':
        return 'disabled';
      case 'connected':
        return 'success';
      default:
        return 'warning';
    }
  }

  function getMCPStatusTitle(server: any): string {
    const status = getMCPHealthLabel(server);
    return server.error
      ? `${status}\n${i18n.t('settings.tools.mcpConnectionIssue')}`
      : status;
  }

  let builtinExpanded = $state(false);
  const builtinReadyCount = $derived(
    (builtinTools as Array<{ runtimeStatus: string }>).filter((t) => t.runtimeStatus === 'ready').length,
  );
  const commandEnvironmentAvailableCount = $derived(
    commandEnvironment?.commands.filter((command: { available: boolean }) => command.available).length ?? 0,
  );

  function toggleBuiltinExpanded() {
    builtinExpanded = !builtinExpanded;
  }

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
          <div class="builtin-summary">
            <button
              type="button"
              class="builtin-summary-toggle"
              onclick={toggleBuiltinExpanded}
              aria-expanded={builtinExpanded}
            >
              <div class="builtin-summary-main">
                <div class="header-title-group">
                  <div class="settings-section-title">{i18n.t('settings.tools.builtinTools')}</div>
                  <span class="builtin-count-tag">{i18n.t('settings.tools.builtinCount', { ready: builtinReadyCount, total: builtinTools.length })}</span>
                </div>
                {#if capabilityDependencies.length > 0}
                  <div
                    class="capability-dependency-strip"
                    aria-label={i18n.t('settings.tools.capabilityDependencySummary')}
                  >
                    {#each capabilityDependencies as dependency (dependency.name)}
                      <span
                        class={`capability-dependency-chip capability-dependency-chip--${getCapabilityDependencyDisplayClass(dependency)}`}
                        title={getCapabilityDependencyTitle(dependency)}
                      >
                        <Icon name={getCapabilityDependencyIcon(dependency.name)} size={11} />
                        <span>{getCapabilityDependencyLabel(dependency.name)}</span>
                        <span class="capability-dependency-status">
                          {getCapabilityDependencyDisplayStatus(dependency)}
                        </span>
                      </span>
                    {/each}
                  </div>
                {/if}
              </div>
              <span class="builtin-expand-icon" class:expanded={builtinExpanded}>
                <Icon name="chevronDown" size={14} />
              </span>
            </button>
            <div class="builtin-summary-actions">
              <button
                type="button"
                class="btn-icon btn-icon--sm"
                class:refreshing={builtinToolsLoading}
                title={i18n.t('settings.tools.refreshBuiltinTools')}
                onclick={() => refreshBuiltinToolCatalog()}
                disabled={builtinToolsLoading}
              >
                <Icon name="refresh" size={12} />
              </button>
            </div>
          </div>
          {#if builtinExpanded}
            <div class="tools-fixed-panel tools-fixed-panel--builtin builtin-tool-panel">
              {#if builtinTools.length === 0}
                <div class="empty-state">
                  <Icon name="tools" size={48} />
                  <p>{i18n.t('settings.tools.noBuiltinTools')}</p>
                </div>
              {:else}
                <div class="builtin-tool-list">
                  {#each builtinTools as tool (tool.name)}
                    <div class="builtin-tool-row">
                      <div class="avatar-squircle builtin-tool-avatar" style="background: rgba(var(--primary-rgb, 0, 122, 255), 0.12); color: var(--primary);">
                        <Icon name={tool.name.includes('process') || tool.name.includes('shell') ? 'terminal' : 'tools'} size={13} />
                      </div>
                      <div class="builtin-tool-identity">
                        <span
                          class="builtin-tool-name"
                          title={`${getBuiltinToolLabel(tool.name)} · ${tool.name}`}
                        >
                          {getBuiltinToolLabel(tool.name)}
                        </span>
                        <span class="tool-code" title={getBuiltinToolScopeLabel(tool)}>{getBuiltinToolScopeLabel(tool)}</span>
                      </div>
                      <div class="builtin-tool-badges">
                        <span class="tool-badge">{getBuiltinToolAccessLabel(tool.accessMode)}</span>
                        <span class="tool-badge" class:tool-badge--risk={tool.riskLevel.toLowerCase() === 'high'}>{getBuiltinToolRiskLabel(tool.riskLevel)}</span>
                        <span class="tool-runtime-status">
                          {getBuiltinToolDisplayStatusLabel(tool)}
                        </span>
                        <span
                          class={`apple-indicator ${getBuiltinToolDisplayStatusClass(tool)}`}
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

        <!-- 命令环境 -->
        <div class="settings-section tools-section command-environment-section">
          <div class="command-environment-panel">
            <div class="command-environment-header">
              <div class="header-title-group">
                <div class="settings-section-title">{i18n.t('settings.tools.commandEnvironment')}</div>
                {#if commandEnvironment}
                  <span class="builtin-count-tag">
                    {i18n.t('settings.tools.commandEnvironmentCount', {
                      available: commandEnvironmentAvailableCount,
                      total: commandEnvironment.commands.length,
                    })}
                  </span>
                {/if}
              </div>
              <button
                type="button"
                class="btn-icon btn-icon--sm"
                class:refreshing={commandEnvironmentLoading}
                title={i18n.t('settings.tools.refreshCommandEnvironment')}
                aria-label={i18n.t('settings.tools.refreshCommandEnvironment')}
                onclick={() => refreshCommandEnvironment()}
                disabled={commandEnvironmentLoading}
              >
                <Icon name="refresh" size={12} />
              </button>
            </div>
            {#if commandEnvironment}
              <div class="command-environment-list">
                {#each commandEnvironment.commands as command (command.name)}
                  <span
                    class:command-available={command.available}
                    class:command-unavailable={!command.available}
                    class="command-environment-command"
                    title={command.path ?? i18n.t('settings.tools.commandUnavailable')}
                  >
                    <span class="command-environment-dot"></span>
                    {command.name}
                  </span>
                {/each}
              </div>
            {:else}
              <div class="command-environment-empty">
                {i18n.t('settings.tools.commandEnvironmentHint')}
              </div>
            {/if}
          </div>
        </div>

        <!-- MCP 工具 -->
        <div class="settings-section tools-section">
          <div class="settings-section-header" style="display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 16px;">
            <div class="header-title-group">
              <div class="settings-section-title">{i18n.t('settings.tools.mcpTools')}</div>
              <div class="settings-section-desc">{i18n.t('settings.tools.mcpDesc')}</div>
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
              <div class="mcp-server-list">
              {#each mcpServers as server (server.id)}
                <div class="mcp-server-item" class:expanded={mcpExpandedServer === server.id}>
                  <div class="mcp-server-row">
                    <button
                      type="button"
                      class="mcp-server-primary"
                      onclick={() => toggleMCPExpand(server.id)}
                      aria-expanded={mcpExpandedServer === server.id}
                    >
                      <span class="avatar-squircle mcp-server-avatar">
                        <Icon name="plug" size={13} />
                      </span>
                      <span class="mcp-server-identity">
                        <span class="mcp-server-name" title={server.name}>{server.name}</span>
                        <span class="mcp-server-subtitle" title={getMCPStatusTitle(server)}>{getMCPServerSubtitle(server)}</span>
                      </span>
                    </button>
                    <div class="mcp-server-actions">
                      <span
                        class="mcp-server-health"
                        title={getMCPStatusTitle(server)}
                      >
                        <span
                          class="apple-indicator"
                          class:success={server.health === 'connected'}
                          class:warning={server.health === 'degraded' || (server.enabled !== false && server.health === 'disconnected' && !server.error)}
                          class:disabled={server.health === 'disabled' || server.enabled === false}
                          class:error={server.enabled !== false && (Boolean(server.error) || !server.health)}
                        ></span>
                      </span>
                      <Toggle
                        checked={server.enabled}
                        title={server.enabled ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
                        onchange={() => toggleMCPServer(server.id, server.enabled)}
                      />
                      <button class="btn-icon btn-icon--sm" title={i18n.t('settings.tools.edit')} onclick={() => openMCPDialog(server)}>
                        <Icon name="edit" size={13} />
                      </button>
                      <button class="btn-icon btn-icon--sm btn-icon--danger" title={i18n.t('settings.tools.delete')} onclick={() => deleteMCPServer(server.id)}>
                        <Icon name="trash" size={13} />
                      </button>
                      <button
                        type="button"
                        class="btn-icon btn-icon--sm mcp-expand-button"
                        title={i18n.t('settings.tools.toolList')}
                        onclick={() => toggleMCPExpand(server.id)}
                        aria-expanded={mcpExpandedServer === server.id}
                      >
                        <span class="mcp-expand-icon" class:expanded={mcpExpandedServer === server.id}>
                          <Icon name="chevronDown" size={14} />
                        </span>
                      </button>
                    </div>
                  </div>

                  {#if mcpExpandedServer === server.id}
                    <div class="mcp-tools-popover">
                      <div class="mcp-tools-header">
                        <div class="mcp-tools-heading">
                          <span>{i18n.t('settings.tools.toolList')}</span>
                          {#if mcpServerTools[server.id]?.length}
                            <span class="mcp-tools-count">{mcpServerTools[server.id].length}</span>
                          {/if}
                        </div>
                        <button class="btn-icon btn-icon--sm" class:refreshing={mcpRefreshingServers.has(server.id)} title={i18n.t('settings.tools.refreshTools')}
                          onclick={() => refreshMCPTools(server.id)} disabled={mcpRefreshingServers.has(server.id)}>
                          <Icon name="refresh" size={12} />
                        </button>
                      </div>
                      <div class="mcp-tools-list">
                        {#if mcpRefreshingServers.has(server.id)}
                          <div class="mcp-tools-empty">{i18n.t('settings.tools.loading')}</div>
                        {:else if mcpServerTools[server.id] && mcpServerTools[server.id].length > 0}
                          {#each mcpServerTools[server.id] as tool}
                            <div class="mcp-tool-item">
                              <div class="mcp-tool-row">
                                <div class="mcp-tool-identity">
                                  <div class="mcp-tool-name" title={tool.name}>{tool.name}</div>
                                  {#if tool.description}
                                    <div class="mcp-tool-description" title={tool.description}>{tool.description}</div>
                                  {/if}
                                </div>
                                {#if tool.description}
                                  <button
                                    class="mcp-tool-desc-btn"
                                    title={i18n.t('settings.tools.viewDesc')}
                                    onmouseenter={(e) => openDesc(tool.description, e)}
                                    onmouseleave={scheduleClose}
                                    onfocus={(e) => openDesc(tool.description, e as unknown as MouseEvent)}
                                    onblur={scheduleClose}
                                  >
                                    <Icon name="info" size={12} />
                                  </button>
                                {/if}
                              </div>
                            </div>
                          {/each}
                        {:else}
                          <div class="mcp-tools-empty">{i18n.t('settings.tools.noToolsHint')}</div>
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

        <!-- Skill 工具 -->
        <div class="settings-section tools-section">
          <div class="settings-section-header" style="display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 16px;">
            <div class="header-title-group">
              <div class="settings-section-title">{i18n.t('settings.tools.skillsTools')}</div>
              <div class="settings-section-desc">{i18n.t('settings.tools.skillsDesc')}</div>
            </div>
            <div class="settings-section-actions">
              <button class="apple-action-btn secondary" onclick={() => checkSkillUpdates()} disabled={skillUpdatesChecking}>
                <Icon name="refresh" size={14} />
                <span>{skillUpdatesChecking ? i18n.t('settings.tools.checkingSkillUpdates') : i18n.t('settings.tools.checkSkillUpdates')}</span>
              </button>
              {#if skillUpdateAvailableCount > 0}
                <button class="apple-action-btn" onclick={() => updateAllSkills()} disabled={skillUpdatesChecking}>
                  <Icon name="download" size={14} />
                  <span>{i18n.t('settings.tools.updateAllSkills', { count: skillUpdateAvailableCount })}</span>
                </button>
              {/if}
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
              <div class="skill-list">
              {#each skills as skill}
                <div class="skill-row" class:disabled={skill.source !== 'custom' && skill.enabled === false}>
                  <div class="skill-avatar">
                    <Icon name={skill.origin === 'local' ? 'folder' : 'tools'} size={13} />
                  </div>
                  <div class="skill-main">
                    <div class="skill-title-line">
                      <span class="skill-name" title={skill.name}>{skill.name}</span>
                      <span class="skill-source-tag">{getSkillSourceLabel(skill)}</span>
                      {#if skill.source !== 'custom' && skill.enabled === false}
                        <span class="skill-status-tag disabled">{i18n.t('settings.tools.disabledLabel')}</span>
                      {:else if skill.source !== 'custom'}
                        <span class="skill-status-tag {getSkillStatusClass(skill)}">{getSkillStatusLabel(skill)}</span>
                      {/if}
                    </div>
                    <p class="skill-desc" title={skill.description}>{skill.description || i18n.t('settings.tools.skillNoDescription')}</p>
                    {#if skill.source !== 'custom'}
                      <div class="skill-meta">
                        {#if skill.version}<span>{i18n.t('settings.tools.skillVersion', { version: skill.version })}</span>{/if}
                        {#if skill.repositoryName}<span title={skill.repositoryName}>{skill.repositoryName}</span>{/if}
                        <span>{skill.lastCheckedAt
                          ? i18n.t('settings.tools.skillCheckedAt', { time: formatSkillCheckedAt(skill.lastCheckedAt) })
                          : i18n.t('settings.tools.skillNeverChecked')}</span>
                      </div>
                    {/if}
                  </div>
                  <div class="skill-row-actions">
                    {#if skill.source !== 'custom'}
                      <Toggle
                        checked={skill.enabled !== false}
                        disabled={skillTogglingIds.has(skill.skillId)}
                        title={skill.enabled !== false ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
                        onchange={(enabled) => toggleSkill(skill.skillId, enabled)}
                      />
                    {/if}
                    {#if skill.source !== 'custom' && (skill.updateAvailable || skill.updateStatus === 'local_modified')}
                      <button class="skill-action-btn primary" onclick={() => updateSkill(skill.skillId)} disabled={skillUpdatingIds.has(skill.skillId)}>
                        <Icon name="refresh" size={13} />
                        <span>{skill.origin === 'local' ? i18n.t('settings.tools.reloadSkill') : i18n.t('settings.tools.updateSkill')}</span>
                      </button>
                    {/if}
                    {#if skill.rollbackAvailable}
                      <button class="skill-action-btn" onclick={() => rollbackSkill(skill.skillId)} disabled={skillUpdatingIds.has(skill.skillId)} title={i18n.t('settings.tools.rollbackSkill')}>
                        <Icon name="refresh" size={13} />
                      </button>
                    {/if}
                    <button class="skill-action-btn danger" title={i18n.t('settings.tools.delete')} onclick={() => deleteSkill(skill)}>
                      <Icon name="trash" size={13} />
                    </button>
                    {#if skillUpdatingIds.has(skill.skillId)}
                      <span class="skill-row-progress"><Icon name="refresh" size={13} /></span>
                    {/if}
                  </div>
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
    container-type: inline-size;
    container-name: tools-tab;
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
    padding-bottom: 12px;
  }

  .command-environment-section {
    padding-top: 12px;
    padding-bottom: 12px;
  }

  .builtin-summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 0;
    background: rgba(var(--foreground-rgb), 0.025);
    border: 1px solid var(--border);
    border-radius: 10px;
    transition: background 0.15s ease, border-color 0.15s ease;
  }

  .builtin-summary:hover {
    background: rgba(var(--foreground-rgb), 0.05);
    border-color: rgba(var(--primary-rgb, 0, 122, 255), 0.35);
  }

  .builtin-summary-toggle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex: 1 1 auto;
    min-width: 0;
    padding: 8px 0 8px 12px;
    background: transparent;
    border: 0;
    color: inherit;
    cursor: pointer;
    font: inherit;
    text-align: left;
  }

  .builtin-summary-toggle:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--primary) 58%, transparent);
    outline-offset: -3px;
    border-radius: 10px;
  }

  .builtin-summary-main {
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 0;
    flex: 1;
  }

  .header-title-group {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 4px 10px;
    min-width: 0;
  }

  .header-title-group .settings-section-title,
  .header-title-group .settings-section-desc {
    margin-bottom: 0;
  }

  .builtin-count-tag {
    font-size: 11px;
    color: var(--foreground-muted);
    padding: 2px 8px;
    border-radius: 999px;
    background: rgba(var(--foreground-rgb), 0.06);
  }

  .capability-dependency-strip {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    flex-wrap: wrap;
  }

  .capability-dependency-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 22px;
    padding: 2px 7px;
    border-radius: 999px;
    border: 1px solid var(--border);
    color: var(--foreground-muted);
    background: rgba(var(--foreground-rgb), 0.04);
    font-size: 10px;
    font-weight: 500;
    white-space: nowrap;
  }

  .capability-dependency-chip--success {
    color: var(--success, #2f855a);
    border-color: color-mix(in srgb, var(--success, #2f855a) 28%, transparent);
    background: color-mix(in srgb, var(--success, #2f855a) 10%, transparent);
  }

  .capability-dependency-chip--warning {
    color: var(--warning, #b7791f);
    border-color: color-mix(in srgb, var(--warning, #b7791f) 32%, transparent);
    background: color-mix(in srgb, var(--warning, #b7791f) 12%, transparent);
  }

  .capability-dependency-chip--error {
    color: var(--error, #d33);
    border-color: color-mix(in srgb, var(--error, #d33) 30%, transparent);
    background: color-mix(in srgb, var(--error, #d33) 10%, transparent);
  }

  .capability-dependency-status {
    opacity: 0.78;
  }

  .builtin-summary-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    padding: 0 8px 0 4px;
  }

  .builtin-expand-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    margin-left: 10px;
    transition: transform 0.2s cubic-bezier(0.3, 0, 0.2, 1);
    color: var(--foreground-muted);
  }

  .builtin-expand-icon.expanded {
    transform: rotate(180deg);
  }

  .command-environment-panel {
    width: 100%;
    box-sizing: border-box;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: rgba(var(--foreground-rgb), 0.025);
  }

  .command-environment-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .command-environment-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 10px;
    margin-top: 8px;
  }

  .command-environment-command {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--foreground-muted);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
  }

  .command-environment-command.command-available {
    color: var(--success, #2f855a);
  }

  .command-environment-command.command-unavailable {
    color: var(--foreground-subtle);
  }

  .command-environment-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: currentColor;
  }

  .command-environment-command.command-unavailable .command-environment-dot {
    background: transparent;
    box-shadow: inset 0 0 0 1px currentColor;
  }

  .command-environment-empty {
    margin-top: 8px;
    color: var(--foreground-subtle);
    font-size: 10px;
  }

  @container tools-tab (max-width: 760px) {
    .builtin-summary {
      align-items: stretch;
    }

    .builtin-summary-toggle {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: center;
      row-gap: 8px;
      padding-top: 10px;
      padding-bottom: 10px;
    }

    .builtin-summary-main {
      display: contents;
    }

    .builtin-summary-main > .header-title-group {
      grid-column: 1;
      grid-row: 1;
    }

    .builtin-expand-icon {
      grid-column: 2;
      grid-row: 1;
      margin-left: 8px;
    }

    .capability-dependency-strip {
      grid-column: 1 / -1;
      grid-row: 2;
      width: 100%;
    }

    .builtin-summary-actions {
      align-self: flex-start;
      padding-top: 8px;
    }
  }

  .builtin-tool-list {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 6px;
  }

  .builtin-tool-panel {
    margin-top: 8px;
  }

  .builtin-tool-row {
    display: grid;
    grid-template-columns: 26px minmax(0, 1fr) auto;
    align-items: center;
    gap: 7px;
    min-width: 0;
    min-height: 46px;
    padding: 5px 8px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: rgba(var(--foreground-rgb), 0.035);
  }

  .builtin-tool-avatar {
    width: 24px;
    height: 24px;
    align-self: center;
  }

  .builtin-tool-identity {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 1px;
    min-width: 0;
  }

  .builtin-tool-name {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: 13px;
    font-weight: 650;
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tool-code {
    min-width: 0;
    overflow: hidden;
    font-size: 9.5px;
    color: var(--foreground-muted);
    line-height: 1.2;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .builtin-tool-badges {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    flex-wrap: nowrap;
    gap: 4px;
    min-width: 0;
  }

  .tool-badge {
    padding: 1px 5px;
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

  @container tools-tab (max-width: 1120px) {
    .builtin-tool-list {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @container tools-tab (max-width: 680px) {
    .builtin-tool-list {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  @container tools-tab (max-width: 560px) {
    .builtin-tool-row {
      grid-template-columns: 26px minmax(0, 1fr);
      min-height: 56px;
    }

    .builtin-tool-badges {
      grid-column: 2;
      justify-content: flex-start;
      flex-wrap: wrap;
    }
  }

  .mcp-tools-popover {
    position: absolute;
    top: calc(100% + 5px);
    right: 8px;
    left: 34px;
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: 9px;
    box-shadow: 0 12px 28px rgba(0, 0, 0, 0.2), 0 1px 3px rgba(0, 0, 0, 0.08);
    z-index: var(--z-popover);
    padding: 0;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    animation: mcp-tools-popover-in 0.14s ease-out;
  }

  .mcp-tools-popover::before {
    position: absolute;
    top: -5px;
    right: 10px;
    width: 8px;
    height: 8px;
    border-top: 1px solid var(--border);
    border-left: 1px solid var(--border);
    background: var(--background);
    content: '';
    transform: rotate(45deg);
  }

  @keyframes mcp-tools-popover-in {
    from {
      opacity: 0;
      transform: translateY(-3px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .mcp-server-list {
    width: 100%;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: rgba(var(--foreground-rgb), 0.025);
  }

  .mcp-server-item {
    position: relative;
    z-index: 1;
  }

  .mcp-server-item + .mcp-server-item {
    border-top: 1px solid var(--border);
  }

  .mcp-server-item.expanded {
    z-index: 100;
  }

  .mcp-server-row {
    display: flex;
    align-items: center;
    min-height: 54px;
    padding: 7px 8px 7px 10px;
    transition: background 0.15s ease;
  }

  .mcp-server-item:first-child .mcp-server-row {
    border-radius: 9px 9px 0 0;
  }

  .mcp-server-item:last-child .mcp-server-row {
    border-radius: 0 0 9px 9px;
  }

  .mcp-server-item:only-child .mcp-server-row {
    border-radius: 9px;
  }

  .mcp-server-row:hover {
    background: rgba(var(--foreground-rgb), 0.045);
  }

  .mcp-server-primary {
    display: flex;
    align-items: center;
    gap: 9px;
    flex: 1 1 auto;
    min-width: 0;
    padding: 0;
    border: 0;
    background: transparent;
    color: inherit;
    cursor: pointer;
    font: inherit;
    text-align: left;
  }

  .mcp-server-primary:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--primary) 58%, transparent);
    outline-offset: 3px;
    border-radius: 6px;
  }

  .mcp-server-avatar {
    flex: 0 0 auto;
    background: rgba(var(--primary-rgb, 0, 122, 255), 0.13);
    color: var(--primary);
  }

  .mcp-server-identity {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 1px;
    min-width: 0;
  }

  .mcp-server-name {
    overflow: hidden;
    color: var(--foreground);
    font-size: 13px;
    font-weight: 650;
    line-height: 1.3;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .mcp-server-subtitle {
    overflow: hidden;
    color: var(--foreground-muted);
    font-size: 10px;
    line-height: 1.3;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .mcp-server-actions {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: 0 0 auto;
    margin-left: 12px;
  }

  .mcp-server-health {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 28px;
  }

  .mcp-expand-button {
    margin-left: 1px;
  }

  .mcp-tools-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    min-height: 36px;
    padding: 5px 8px 5px 11px;
    border-bottom: 1px solid var(--border);
  }

  .mcp-tools-heading {
    display: flex;
    align-items: center;
    gap: 7px;
    color: var(--foreground);
    font-size: 11px;
    font-weight: 600;
  }

  .mcp-tools-count {
    min-width: 18px;
    padding: 1px 5px;
    border-radius: 999px;
    background: rgba(var(--foreground-rgb), 0.07);
    color: var(--foreground-muted);
    font-size: 9px;
    font-weight: 600;
    line-height: 16px;
    text-align: center;
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

  .avatar-squircle {
    width: 24px;
    height: 24px;
    border-radius: 7px;
    display: flex;
    align-items: center;
    justify-content: center;
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

  @container tools-tab (max-width: 640px) {
    .mcp-server-row {
      min-height: 52px;
      padding-left: 8px;
      padding-right: 6px;
    }

    .mcp-server-actions {
      gap: 2px;
      margin-left: 8px;
    }

    .mcp-server-health {
      width: 14px;
    }

    .mcp-server-avatar {
      width: 22px;
      height: 22px;
    }

    .mcp-tools-popover {
      right: 5px;
      left: 5px;
    }
  }

  .mcp-tools-list {
    display: flex;
    flex-direction: column;
    max-height: min(280px, 42vh);
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 5px;
  }

  /* MCP 工具项样式 */
  .mcp-tool-item {
    position: relative;
    padding: 7px 8px;
    border-radius: 6px;
    transition: background var(--transition-fast);
  }
  .mcp-tool-item:hover {
    background: rgba(var(--foreground-rgb), 0.045);
  }
  .mcp-tool-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .mcp-tool-identity {
    flex: 1;
    min-width: 0;
  }
  .mcp-tool-name {
    overflow: hidden;
    color: var(--foreground);
    font-size: 11px;
    font-weight: 600;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mcp-tool-description {
    display: -webkit-box;
    overflow: hidden;
    margin-top: 2px;
    color: var(--foreground-muted);
    font-size: 9.5px;
    line-height: 1.35;
    line-clamp: 1;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 1;
  }
  .mcp-tools-empty {
    padding: 14px 10px;
    color: var(--foreground-muted);
    font-size: 10px;
    text-align: center;
  }

  @media (prefers-reduced-motion: reduce) {
    .mcp-tools-popover {
      animation: none;
    }
  }

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
    opacity: 0.62;
    transform: scale(0.9);
  }
  .mcp-tool-desc-btn:hover,
  .mcp-tool-desc-btn:focus-visible {
    background: var(--surface-hover);
    border-color: var(--primary);
    color: var(--foreground);
    opacity: 1;
  }

  /* Skill 卡片内部 */
  .skill-list {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    background: color-mix(in srgb, var(--surface-1) 94%, transparent);
  }
  .skill-row {
    display: grid;
    grid-template-columns: 30px minmax(0, 1fr) auto;
    align-items: center;
    gap: 10px;
    min-height: 64px;
    padding: 9px 12px;
    border-bottom: 1px solid var(--border);
  }
  .skill-row:last-child { border-bottom: 0; }
  .skill-row:hover { background: rgba(var(--foreground-rgb), 0.025); }
  .skill-row.disabled .skill-avatar {
    background: var(--surface-3);
    color: var(--foreground-subtle);
  }
  .skill-row.disabled .skill-desc,
  .skill-row.disabled .skill-meta { opacity: 0.72; }
  .skill-avatar { width: 28px; height: 28px; border-radius: 7px; display: flex; align-items: center; justify-content: center; flex-shrink: 0; background: rgba(var(--success-rgb, 52, 199, 89), 0.1); color: var(--success); }
  .skill-main { min-width: 0; }
  .skill-title-line { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .skill-name { max-width: 42%; font-size: 13px; font-weight: 650; color: var(--foreground); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .skill-source-tag, .skill-status-tag { font-size: 9px; line-height: 16px; padding: 0 6px; border-radius: 4px; white-space: nowrap; flex-shrink: 0; }
  .skill-source-tag { border: 1px solid var(--border); color: var(--foreground-muted); }
  .skill-status-tag.success { color: var(--success); background: rgba(var(--success-rgb, 52, 199, 89), 0.1); }
  .skill-status-tag.warning { color: var(--warning); background: rgba(var(--warning-rgb, 255, 149, 0), 0.11); }
  .skill-status-tag.modified { color: var(--primary); background: rgba(var(--primary-rgb, 0, 122, 255), 0.1); }
  .skill-status-tag.error { color: var(--error); background: rgba(var(--error-rgb, 255, 59, 48), 0.1); }
  .skill-status-tag.disabled { color: var(--foreground-muted); background: var(--surface-3); }
  .skill-desc { margin: 3px 0 0; font-size: 11px; color: var(--foreground-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .skill-meta { display: flex; gap: 10px; margin-top: 3px; min-width: 0; font-size: 10px; color: var(--foreground-subtle); }
  .skill-meta span { min-width: 0; max-width: 38%; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .skill-row-actions { display: flex; align-items: center; justify-content: flex-end; gap: 4px; }
  .skill-action-btn { min-width: 28px; height: 28px; padding: 0 8px; display: inline-flex; align-items: center; justify-content: center; gap: 5px; border: 1px solid var(--border); border-radius: 6px; background: transparent; color: var(--foreground-muted); font: inherit; font-size: 11px; cursor: pointer; }
  .skill-action-btn:hover { color: var(--foreground); background: rgba(var(--foreground-rgb), 0.05); }
  .skill-action-btn.primary { color: var(--primary); border-color: color-mix(in srgb, var(--primary) 35%, var(--border)); }
  .skill-action-btn.danger:hover { color: var(--error); background: rgba(var(--error-rgb, 255, 59, 48), 0.08); }
  .skill-action-btn:disabled { opacity: 0.45; cursor: wait; }
  .skill-row-progress { display: inline-flex; color: var(--primary); animation: spin 0.9s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }

  @container tools-tab (max-width: 640px) {
    .settings-section-header {
      align-items: flex-start !important;
      flex-direction: column;
      gap: 10px;
    }
    .settings-section-actions {
      display: flex;
      flex-wrap: wrap;
      width: 100%;
    }
    .skill-row {
      grid-template-columns: 28px minmax(0, 1fr);
      align-items: start;
      padding: 10px;
    }
    .skill-row-actions {
      grid-column: 2;
      justify-content: flex-start;
    }
    .skill-name { max-width: 55%; }
    .skill-meta { flex-wrap: wrap; gap: 3px 8px; }
    .skill-meta span { max-width: 100%; }
  }

  /* 空状态 */
  
  
  
</style>

<script lang="ts">
  import Disclosure from './Disclosure.svelte';
  import SettingsMcpToolsSection from './SettingsMcpToolsSection.svelte';
  import FileName from './FileName.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { getAgentSkillDetail } from '../web/agent-api';
  import { formatSkillFileSize, splitSkillInstruction } from '../lib/skill-instruction';
  import type { SkillDetailDto } from '../shared/rust-backend-types';
  import type { IconName } from '../lib/icons';
  import {
    getBuiltinToolFallbackLabel,
    getCapabilityDependencyFallbackLabel,
  } from '../shared/tool-display';

  let {
    mcpServers,
    mcpServersHydrated,
    mcpServersLoading,
    mcpServersError,
    retryMcpServers,
    mcpExpandedServer,
    mcpServerTools,
    mcpRefreshingServers,
    mcpToolErrors,
    mcpChangingServers,
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
  }: {
    mcpServers: any[];
    mcpServersHydrated: boolean;
    mcpServersLoading: boolean;
    mcpServersError: string;
    retryMcpServers: () => void;
    mcpExpandedServer: string | null;
    mcpServerTools: Record<string, any[]>;
    mcpRefreshingServers: Set<string>;
    mcpToolErrors: Record<string, string>;
    mcpChangingServers: Set<string>;
    builtinToolsLoading: boolean;
    builtinTools: Array<{
      name: string;
      category: string;
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
  } = $props();

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

  // Skill 详情：行内展开，首次展开时按需读取说明文件全文与文件清单。
  type SkillDetailState =
    | { status: 'loading' }
    | { status: 'error'; message: string }
    | { status: 'ready'; detail: SkillDetailDto };
  let skillExpandedId = $state<string | null>(null);
  let skillDetails = $state<Record<string, SkillDetailState>>({});
  let skillRawView = $state<Record<string, boolean>>({});

  async function loadSkillDetail(skillId: string): Promise<void> {
    skillDetails[skillId] = { status: 'loading' };
    try {
      skillDetails[skillId] = { status: 'ready', detail: await getAgentSkillDetail(skillId) };
    } catch (error) {
      skillDetails[skillId] = {
        status: 'error',
        message: error instanceof Error ? error.message : i18n.t('settings.tools.skillDetailLoadFailed'),
      };
    }
  }

  function toggleSkillDetail(skillId: string): void {
    if (skillExpandedId === skillId) {
      skillExpandedId = null;
      return;
    }
    skillExpandedId = skillId;
    // 说明文件可能在磁盘上被改过：每次展开都重新读取，不缓存陈旧内容。
    void loadSkillDetail(skillId);
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
    return formatBuiltinToolFallbackLabel(name);
  }

  function getBuiltinCategoryLabel(category: string): string {
    const suffix = category
      .split('_')
      .filter(Boolean)
      .map((part, index) => index === 0 ? part : part.charAt(0).toUpperCase() + part.slice(1))
      .join('');
    const key = suffix ? `settings.tools.builtinCategory.${suffix}` : '';
    const translated = key ? i18n.t(key) : '';
    return translated && translated !== key
      ? translated
      : getBuiltinToolFallbackLabel(category);
  }

  function getBuiltinCategoryIcon(category: string): IconName {
    switch (category) {
      case 'filesystem': return 'folder';
      case 'code_navigation': return 'code';
      case 'process': return 'terminal';
      case 'web': return 'globe';
      case 'browser': return 'globe';
      case 'knowledge': return 'database';
      case 'git': return 'git-branch';
      case 'session_goal': return 'target';
      case 'tooling': return 'settings';
      default: return 'tools';
    }
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
        return i18n.t('settings.status.checking');
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
    const displayStatus = getCapabilityDependencyStatusLabel(dependency.status);
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

  const builtinCapabilityDependencies = $derived(
    capabilityDependencies.filter(dependency => dependency.name !== 'mcp_servers'),
  );
  let builtinExpanded = $state(false);
  const builtinToolGroups = $derived.by(() => {
    const groups = new Map<string, typeof builtinTools>();
    for (const tool of builtinTools) {
      const category = tool.category || 'uncategorized';
      const items = groups.get(category) ?? [];
      items.push(tool);
      groups.set(category, items);
    }
    return Array.from(groups, ([category, tools]) => ({
      category,
      tools,
      readyCount: tools.filter((tool: (typeof builtinTools)[number]) => tool.runtimeStatus === 'ready').length,
    }));
  });
  const builtinReadyCount = $derived(
    builtinToolGroups.filter((group) => group.readyCount === group.tools.length).length,
  );
  const commandEnvironmentAvailableCount = $derived(
    commandEnvironment?.commands.filter((command: { available: boolean }) => command.available).length ?? 0,
  );

  function toggleBuiltinExpanded() {
    builtinExpanded = !builtinExpanded;
  }

</script>

<div class="apple-manager tools-manager">
  <div class="apple-scroller-proxy">
        <SettingsMcpToolsSection servers={mcpServers} hydrated={mcpServersHydrated} loading={mcpServersLoading} listError={mcpServersError} retryServers={retryMcpServers}
          expandedServer={mcpExpandedServer} tools={mcpServerTools} refreshing={mcpRefreshingServers}
          errors={mcpToolErrors} changing={mcpChangingServers} openEditor={openMCPDialog}
          toggleExpanded={toggleMCPExpand} healthLabel={getMCPHealthLabel} toggleServer={toggleMCPServer}
          deleteServer={deleteMCPServer} refreshTools={refreshMCPTools} />

        <!-- Skill 工具 -->
        <div class="settings-section tools-section">
          <div class="settings-section-header">
            <div>
              <div class="settings-section-title">{i18n.t('settings.tools.skillsTools')}</div>

            </div>
            <div class="settings-section-actions">
              <button class="btn btn--secondary btn--sm tools-section-refresh-action" onclick={() => checkSkillUpdates()} disabled={skillUpdatesChecking}>
                {#if skillUpdatesChecking}<Icon name="loader" size={16} class="tools-refresh-icon spinning" />{:else}<Icon name="refresh" size={16} class="tools-refresh-icon" />{/if}
                <span>{skillUpdatesChecking ? i18n.t('settings.tools.checkingSkillUpdates') : i18n.t('settings.tools.checkSkillUpdates')}</span>
              </button>
              {#if skillUpdateAvailableCount > 0}
                <button class="btn btn--secondary btn--sm" onclick={() => updateAllSkills()} disabled={skillUpdatesChecking}>
                  <Icon name="download" size={14} />
                  <span>{i18n.t('settings.tools.updateAllSkills', { count: skillUpdateAvailableCount })}</span>
                </button>
              {/if}
              <button class="btn btn--secondary btn--sm" onclick={() => openRepoDialog()}>
                <Icon name="grid" size={14} />
                <span>{i18n.t('settings.tools.manageRepos')}</span>
              </button>
              <button class="btn btn--primary btn--sm" onclick={() => openSkillLibraryDialog()}>
                <Icon name="plus" size={14} />
                <span>{i18n.t('settings.tools.installSkill')}</span>
              </button>
            </div>
          </div>
          <div class="tools-fixed-panel tools-fixed-panel--skills">
            {#if skills.length === 0}
              <div class="settings-empty">
                <Icon name="tools" size={16} />
                <div><div class="settings-empty-title">{i18n.t('settings.tools.noSkills')}</div>{i18n.t('settings.tools.noSkillsHint')}</div>
              </div>
            {:else}
              <div class="skill-list">
              {#each skills as skill (skill.skillId)}
                <div class="skill-item">
                <div class="skill-row" class:disabled={skill.source !== 'custom' && skill.enabled === false}>
                  <button type="button" class="settings-disclosure-trigger skill-primary"
                    aria-expanded={skillExpandedId === skill.skillId} aria-controls={`skill-detail-${skill.skillId}`}
                    onclick={() => toggleSkillDetail(skill.skillId)}>
                  <span class="settings-disclosure-chevron"><Icon name="chevron-right" size={14} /></span>
                  <span class="skill-avatar">
                    <Icon name={skill.origin === 'local' ? 'folder' : 'tools'} size={13} />
                  </span>
                  <span class="skill-main">
                    <span class="skill-title-line">
                      <span class="skill-name" title={skill.name}>{skill.name}</span>
                      <span class="skill-source-tag">{getSkillSourceLabel(skill)}</span>
                      {#if skill.source !== 'custom' && skill.enabled === false}
                        <span class="skill-status-tag disabled">{i18n.t('settings.tools.disabledLabel')}</span>
                      {:else if skill.source !== 'custom'}
                        <span class="skill-status-tag {getSkillStatusClass(skill)}">{getSkillStatusLabel(skill)}</span>
                      {/if}
                    </span>
                    <span class="skill-desc" title={skill.description}>{skill.description || i18n.t('settings.tools.skillNoDescription')}</span>
                    {#if skill.source !== 'custom'}
                      <span class="skill-meta">
                        <span>{i18n.t('settings.tools.skillVersion', { version: skill.version || i18n.t('common.notProvided') })}</span>
                        {#if skill.repositoryName}<span title={skill.repositoryName}>{skill.repositoryName}</span>{/if}
                        <span>{skill.lastCheckedAt
                          ? i18n.t('settings.tools.skillCheckedAt', { time: formatSkillCheckedAt(skill.lastCheckedAt) })
                          : i18n.t('settings.tools.skillNeverChecked')}</span>
                      </span>
                    {/if}
                  </span>
                  </button>
                  <div class="skill-row-actions">
                    {#if skill.source !== 'custom'}
                      <Toggle
                        checked={skill.enabled !== false}
                        ariaLabel={skill.name}
                        disabled={skillTogglingIds.has(skill.skillId)}
                        title={skill.enabled !== false ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
                        onchange={(enabled) => toggleSkill(skill.skillId, enabled)}
                      />
                    {/if}
                    {#if skill.source !== 'custom' && !skill.builtin && (skill.updateAvailable || skill.updateStatus === 'local_modified')}
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
                    {#if skill.builtin}
                      <span class="skill-builtin-badge" data-skill-builtin="1" title={i18n.t('settings.tools.builtinSkillHint')}>{i18n.t('settings.tools.builtinSkill')}</span>
                    {:else}
                      <button class="skill-action-btn danger" title={i18n.t('settings.tools.delete')} onclick={() => deleteSkill(skill)}>
                        <Icon name="trash" size={13} />
                      </button>
                    {/if}
                    {#if skillUpdatingIds.has(skill.skillId)}
                      <span class="skill-row-progress"><Icon name="refresh" size={13} /></span>
                    {/if}
                  </div>
                </div>

                <Disclosure expanded={skillExpandedId === skill.skillId}>
                  {@const state = skillDetails[skill.skillId]}
                  <div class="skill-detail" id={`skill-detail-${skill.skillId}`}>
                    {#if !state || state.status === 'loading'}
                      <div class="skill-detail-note"><Icon name="loader" size={16} class="spinning" />{i18n.t('settings.tools.loading')}</div>
                    {:else if state.status === 'error'}
                      <div class="skill-detail-note skill-detail-note--error" role="alert">{state.message}</div>
                    {:else}
                      {@const parts = splitSkillInstruction(state.detail.instruction)}
                      <div class="skill-detail-head">
                        <div class="skill-detail-title">
                          <span class="skill-detail-file">{state.detail.instructionFile}</span>
                          <span class="skill-detail-size">{formatSkillFileSize(state.detail.instructionBytes)}</span>
                        </div>
                        <div class="skill-detail-tabs" role="tablist">
                          <button type="button" role="tab" class="skill-detail-tab" class:active={!skillRawView[skill.skillId]} aria-selected={!skillRawView[skill.skillId]} onclick={() => (skillRawView[skill.skillId] = false)}>{i18n.t('settings.tools.skillDetailRendered')}</button>
                          <button type="button" role="tab" class="skill-detail-tab" class:active={Boolean(skillRawView[skill.skillId])} aria-selected={Boolean(skillRawView[skill.skillId])} onclick={() => (skillRawView[skill.skillId] = true)}>{i18n.t('settings.tools.skillDetailRaw')}</button>
                        </div>
                      </div>
                      {#if parts.meta.length > 0 && !skillRawView[skill.skillId]}
                        <dl class="skill-detail-meta">
                          {#each parts.meta as item (item.key)}
                            <dt>{item.key}</dt>
                            <dd title={item.value}>{item.value}</dd>
                          {/each}
                        </dl>
                      {/if}
                      <div class="skill-detail-body">
                        {#if skillRawView[skill.skillId]}
                          <pre class="skill-detail-raw">{state.detail.instruction}</pre>
                        {:else}
                          <MarkdownContent content={parts.body} isStreaming={false} />
                        {/if}
                      </div>
                      {#if state.detail.truncated}
                        <div class="skill-detail-note">{i18n.t('settings.tools.skillDetailTruncated')}</div>
                      {/if}
                      {#if state.detail.files.length > 0}
                        <div class="skill-detail-files">
                          <div class="skill-detail-files-title">{i18n.t('settings.tools.skillDetailFiles', { count: state.detail.files.length })}</div>
                          <ul>
                            {#each state.detail.files as file (file.path)}
                              <li><code title={file.path}><FileName name={file.path} /></code><span>{formatSkillFileSize(file.size)}</span></li>
                            {/each}
                          </ul>
                        </div>
                      {/if}
                    {/if}
                  </div>
                </Disclosure>
                </div>
              {/each}
              </div>
            {/if}
          </div>
        </div>
        <!-- 内置工具 -->
        <div class="settings-section tools-section builtin-section">
          <div class="settings-section-header">
            <button type="button" class="settings-disclosure-trigger builtin-disclosure-toggle"
              onclick={toggleBuiltinExpanded} aria-expanded={builtinExpanded} aria-controls="builtin-tool-directory">
              <span class="settings-disclosure-chevron"><Icon name="chevron-right" size={14} /></span>
              <span class="settings-section-title">
                {i18n.t('settings.tools.builtinTools')}
                <span class="builtin-count-tag">{i18n.t('settings.tools.builtinCount', { ready: builtinReadyCount, total: builtinToolGroups.length })}</span>
              </span>
              <span class="builtin-disclosure-hint">{builtinExpanded ? i18n.t('settings.tools.collapse') : i18n.t('settings.tools.expand')}</span>
            </button>
            <div class="settings-section-actions">
              <button
                type="button"
                class="btn-icon tools-section-refresh"
                class:refreshing={builtinToolsLoading}
                title={i18n.t('settings.tools.refreshBuiltinTools')}
                aria-label={i18n.t('settings.tools.refreshBuiltinTools')}
                onclick={() => refreshBuiltinToolCatalog()}
                disabled={builtinToolsLoading}
              >
                <Icon name="refresh" size={16} class="tools-refresh-icon" />
              </button>
            </div>
          </div>
          {#if builtinCapabilityDependencies.length > 0}
            <div class="capability-dependency-strip" aria-label={i18n.t('settings.tools.capabilityDependencySummary')}>
              {#each builtinCapabilityDependencies as dependency (dependency.name)}
                <span
                  class={`capability-dependency-chip capability-dependency-chip--${getCapabilityDependencyClass(dependency.status)}`}
                  title={getCapabilityDependencyTitle(dependency)}
                >
                  <Icon name={getCapabilityDependencyIcon(dependency.name)} size={11} />
                  <span>{getCapabilityDependencyLabel(dependency.name)}</span>
                  <span class="capability-dependency-status">
                    {getCapabilityDependencyStatusLabel(dependency.status)}
                  </span>
                </span>
              {/each}
            </div>
          {/if}
          <Disclosure expanded={builtinExpanded}>
            <div class="tools-fixed-panel tools-fixed-panel--builtin builtin-tool-panel" id="builtin-tool-directory">
              {#if builtinTools.length === 0}
                <div class="settings-empty"><span>{i18n.t('settings.tools.noBuiltinTools')}</span></div>
              {:else}
                <div class="builtin-category-list">
                  {#each builtinToolGroups as group (group.category)}
                    <details class="builtin-category-group">
                      <summary class="settings-disclosure-trigger builtin-category-summary">
                        <span class="settings-disclosure-chevron"><Icon name="chevron-right" size={14} /></span>
                        <div class="avatar-squircle builtin-tool-avatar" style="background: rgba(var(--primary-rgb, 0, 122, 255), 0.12); color: var(--primary);">
                          <Icon name={getBuiltinCategoryIcon(group.category)} size={13} />
                        </div>
                        <div class="builtin-tool-identity">
                          <span class="builtin-tool-name">{getBuiltinCategoryLabel(group.category)}</span>
                          <span class="tool-code">{i18n.t('settings.tools.builtinCategoryActionCount', { count: group.tools.length })}</span>
                        </div>
                        <div class="builtin-tool-badges">
                          <span class="tool-runtime-status">
                            {i18n.t('settings.tools.builtinCategoryReadyCount', { ready: group.readyCount, total: group.tools.length })}
                          </span>
                          <span class={`apple-indicator ${group.readyCount === group.tools.length ? 'success' : group.readyCount > 0 ? 'warning' : 'error'}`}></span>
                        </div>
                      </summary>
                      <div class="builtin-tool-list">
                        {#each group.tools as tool (tool.name)}
                          <div class="builtin-tool-row">
                            <div class="avatar-squircle builtin-tool-avatar" style="background: rgba(var(--primary-rgb, 0, 122, 255), 0.12); color: var(--primary);">
                              <Icon name={getBuiltinCategoryIcon(group.category)} size={13} />
                            </div>
                            <div class="builtin-tool-identity">
                              <span
                                class="builtin-tool-name"
                                title={`${getBuiltinToolLabel(tool.name)}: ${tool.name}`}
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
                    </details>
                  {/each}
                </div>
              {/if}
            </div>
          </Disclosure>
        </div>

        <!-- 命令环境 -->
        <div class="settings-section tools-section command-environment-section">
          <div class="settings-section-header">
            <div>
              <div class="settings-section-title">
                {i18n.t('settings.tools.commandEnvironment')}
                {#if commandEnvironment}
                  <span class="builtin-count-tag">
                    {i18n.t('settings.tools.commandEnvironmentCount', {
                      available: commandEnvironmentAvailableCount,
                      total: commandEnvironment.commands.length,
                    })}
                  </span>
                {/if}
              </div>
              <div class="settings-section-desc">{i18n.t('settings.tools.commandEnvironmentDesc')}</div>
            </div>
            <div class="settings-section-actions">
              <button
                type="button"
                class="btn-icon tools-section-refresh"
                class:refreshing={commandEnvironmentLoading}
                title={i18n.t('settings.tools.refreshCommandEnvironment')}
                aria-label={i18n.t('settings.tools.refreshCommandEnvironment')}
                onclick={() => refreshCommandEnvironment()}
                disabled={commandEnvironmentLoading}
              >
                <Icon name="refresh" size={16} class="tools-refresh-icon" />
              </button>
            </div>
          </div>
          <div class="command-environment-panel">
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

    </div>
</div>

<style>
  .tools-manager {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    container-type: inline-size;
    container-name: tools-tab;
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
    overflow: visible;
    padding-right: 0;
  }

  .tools-fixed-panel .apple-grid {
    min-height: 0;
  }

  .builtin-count-tag {
    margin-left: 8px;
    vertical-align: 1px;
    font-weight: var(--font-normal);
    font-size: var(--text-xs);
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
    padding: 10px 12px;
    border: var(--magi-hairline) solid var(--border);
    border-radius: 10px;
  }

  .capability-dependency-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 22px;
    padding: 2px 7px;
    border-radius: 999px;
    border: var(--magi-hairline) solid var(--border);
    color: var(--foreground-muted);
    background: rgba(var(--foreground-rgb), 0.04);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
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

  .builtin-disclosure-toggle { flex: 1; flex-wrap: wrap; }
  .builtin-disclosure-hint { color: var(--foreground-muted); font-size: var(--text-xs); }

  .command-environment-panel {
    width: 100%;
    box-sizing: border-box;
    padding: 12px;
    border: var(--magi-hairline) solid var(--border);
    border-radius: 10px;
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
    font-size: var(--text-xs);
  }

  .command-environment-command.command-available {
    color: var(--success, #2f855a);
  }

  .command-environment-command.command-unavailable {
    color: var(--foreground-muted);
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
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .builtin-category-list {
    display: flex;
    flex-direction: column;
    border: var(--magi-hairline) solid var(--border);
    border-radius: 8px;
  }

  .builtin-category-group {
    min-width: 0;
    border: 0;
    border-radius: 0;
    background: transparent;
  }

  .builtin-category-group + .builtin-category-group {
    border-top: var(--magi-hairline) solid var(--border);
  }

  .builtin-category-summary {
    display: grid;
    grid-template-columns: 14px 28px minmax(0, 1fr) auto;
    align-items: center;
    gap: 9px;
    min-height: 52px;
    padding: 7px 10px;
  }

  .builtin-category-group > .builtin-tool-list {
    padding: 3px 10px 5px 45px;
    border-top: var(--magi-hairline) solid var(--border);
    background: color-mix(in srgb, var(--surface-2) 68%, transparent);
    animation: builtin-tool-list-in 0.14s ease-out;
  }

  .builtin-tool-list {
    display: flex;
    flex-direction: column;
  }

  .builtin-tool-panel {
    margin-top: 8px;
  }

  .builtin-tool-row {
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
    min-width: 0;
    min-height: 44px;
    padding: 5px 4px;
    border: 0;
    border-bottom: var(--magi-hairline) solid color-mix(in srgb, var(--border) 72%, transparent);
    border-radius: 0;
    background: transparent;
    transition: background 0.15s ease;
  }

  .builtin-tool-row:last-child {
    border-bottom: 0;
  }

  .builtin-tool-row:hover {
    background: rgba(var(--foreground-rgb), 0.035);
  }

  .builtin-tool-list .builtin-tool-avatar {
    width: 22px;
    height: 22px;
    border-radius: 6px;
    color: var(--foreground-muted) !important;
    background: rgba(var(--foreground-rgb), 0.055) !important;
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
    font-weight: calc(var(--font-semibold) + 50);
    line-height: 1.25;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tool-code {
    min-width: 0;
    overflow: hidden;
    font-size: var(--text-xs);
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
    border-radius: 4px;
    background: rgba(var(--foreground-rgb), 0.06);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
  }

  .tool-badge--risk {
    color: var(--warning, #b7791f);
    background: rgba(181, 118, 20, 0.12);
  }

  .tool-runtime-status {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  @keyframes builtin-tool-list-in {
    from {
      opacity: 0;
      transform: translateY(-2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .builtin-category-group > .builtin-tool-list {
      animation: none;
    }
  }

  @container tools-tab (max-width: 560px) {
    .builtin-category-group > .builtin-tool-list {
      padding-left: 12px;
    }

    .builtin-tool-row {
      grid-template-columns: 24px minmax(0, 1fr);
      min-height: 56px;
    }

    .builtin-tool-badges {
      grid-column: 2;
      justify-content: flex-start;
      flex-wrap: wrap;
    }
  }

  /* Skill 卡片内部 */
  .skill-list {
    display: flex;
    flex-direction: column;
    border: var(--magi-hairline) solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .skill-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 10px;
    min-height: 64px;
    padding: 9px 12px;
  }
  .skill-item { border-bottom: var(--magi-hairline) solid var(--border); }
  .skill-item:last-child { border-bottom: 0; }
  .skill-primary { inline-size: 100%; }
  .skill-row.disabled .skill-avatar {
    background: var(--surface-3);
    color: var(--foreground-muted);
  }
  .skill-row.disabled .skill-desc,
  .skill-row.disabled .skill-meta { opacity: 0.72; }
  .skill-avatar { width: 28px; height: 28px; border-radius: 7px; display: flex; align-items: center; justify-content: center; flex-shrink: 0; background: rgba(var(--success-rgb, 52, 199, 89), 0.1); color: var(--success); }
  .skill-main { display: block; min-width: 0; flex: 1; }
  .skill-title-line { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .skill-name { max-width: 42%; font-size: 13px; font-weight: calc(var(--font-semibold) + 50); color: var(--foreground); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .skill-source-tag, .skill-status-tag { font-size: var(--text-xs); line-height: 16px; padding: 0 6px; border-radius: 4px; white-space: nowrap; flex-shrink: 0; }
  .skill-source-tag { border: var(--magi-hairline) solid var(--border); color: var(--foreground-muted); }
  .skill-status-tag.success { color: var(--success); background: rgba(var(--success-rgb, 52, 199, 89), 0.1); }
  .skill-status-tag.warning { color: var(--warning); background: rgba(var(--warning-rgb, 255, 149, 0), 0.11); }
  .skill-status-tag.modified { color: var(--primary); background: rgba(var(--primary-rgb, 0, 122, 255), 0.1); }
  .skill-status-tag.error { color: var(--error); background: rgba(var(--error-rgb, 255, 59, 48), 0.1); }
  .skill-status-tag.disabled { color: var(--foreground-muted); background: var(--surface-3); }
  .skill-desc { display: block; margin: 3px 0 0; font-size: var(--text-xs); color: var(--foreground-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .skill-meta { display: flex; gap: 10px; margin-top: 3px; min-width: 0; font-size: var(--text-xs); color: var(--foreground-muted); }
  .skill-meta span { min-width: 0; max-width: 38%; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .skill-builtin-badge {
    padding: 1px 8px;
    border-radius: 999px;
    font-size: var(--text-xs);
    background: var(--surface-muted, rgba(127, 127, 127, 0.14));
    opacity: 0.8;
  }

  .skill-row-actions { display: flex; align-items: center; justify-content: flex-end; gap: 4px; }
  .skill-action-btn { min-width: 28px; height: 28px; padding: 0 8px; display: inline-flex; align-items: center; justify-content: center; gap: 5px; border: var(--magi-hairline) solid var(--border); border-radius: 6px; background: transparent; color: var(--foreground-muted); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .skill-action-btn:hover { color: var(--foreground); background: rgba(var(--foreground-rgb), 0.05); }
  .skill-action-btn.primary { color: var(--primary); border-color: color-mix(in srgb, var(--primary) 35%, var(--border)); }
  .skill-action-btn.danger:hover { color: var(--error); background: rgba(var(--error-rgb, 255, 59, 48), 0.08); }
  .skill-action-btn:disabled { opacity: 0.45; cursor: wait; }
  .skill-detail {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 10px 12px 12px 52px;
    border-top: 1px dashed color-mix(in srgb, var(--border) 70%, transparent);
    background: color-mix(in srgb, var(--surface-2, var(--surface-1)) 55%, transparent);
  }
  .skill-detail-note { color: var(--foreground-muted); font-size: 12px; }
  .skill-detail-note--error { color: var(--error); }
  .skill-detail-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .skill-detail-title { display: inline-flex; align-items: baseline; gap: 8px; min-width: 0; }
  .skill-detail-file { font-family: var(--font-mono, monospace); font-size: 12px; font-weight: var(--font-semibold); color: var(--foreground); }
  .skill-detail-size { font-size: var(--text-xs); color: var(--foreground-muted); }
  .skill-detail-tabs { display: inline-flex; gap: 2px; padding: 2px; border-radius: 999px; background: color-mix(in srgb, var(--foreground) 6%, transparent); }
  .skill-detail-tab { padding: 1px 12px; border: 0; border-radius: 999px; background: transparent; color: var(--foreground-muted); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .skill-detail-tab.active { background: var(--surface-1); color: var(--primary); font-weight: var(--font-semibold); box-shadow: var(--shadow-sm); }
  .skill-detail-meta { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 2px 12px; margin: 0; font-size: 12px; }
  .skill-detail-meta dt { color: var(--foreground-muted); }
  .skill-detail-meta dd { margin: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground); }
  .skill-detail-body {
    min-width: 0;
    overflow-wrap: anywhere;
    padding: 10px 12px;
    border: var(--magi-hairline) solid var(--border);
    border-radius: 8px;
    background: var(--surface-1);
    font-size: 13px;
  }
  .skill-detail-raw { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font-family: var(--font-mono, monospace); font-size: 12px; line-height: 1.55; color: var(--foreground); }
  .skill-detail-files-title { margin-bottom: 4px; font-size: var(--text-xs); font-weight: var(--font-semibold); color: var(--foreground-muted); }
  .skill-detail-files ul { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 2px; }
  .skill-detail-files li { display: flex; justify-content: space-between; gap: 12px; font-size: 12px; }
  .skill-detail-files code { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground); }
  .skill-detail-files li span { flex: 0 0 auto; color: var(--foreground-muted); }
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
      grid-template-columns: minmax(0, 1fr);
      align-items: start;
      padding: 10px;
    }
    .skill-row-actions {
      grid-column: 1;
      padding-inline-start: 58px;
      justify-content: flex-start;
    }
    .skill-name { max-width: 55%; }
    .skill-meta { flex-wrap: wrap; gap: 3px 8px; }
    .skill-meta span { max-width: 100%; }
  }

  /* 空状态 */

</style>

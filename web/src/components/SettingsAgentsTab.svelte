<script lang="ts">
  import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
  import type { ProfessionalCapabilitySummary, RoleTemplate } from '../shared/types/role-templates';
  import { isAgentBindingOperational, resolveSelectableRegistryEngines } from '../shared/model-governance';
  import { i18n } from '../stores/i18n.svelte';
  import { AgentApiError } from '../web/agent-api';
  import Icon from './Icon.svelte';
  import EnginePicker from './EnginePicker.svelte';

  let {
    roleTemplates,
    registryAgents,
    registryEngines,
    inheritModelLabel = '',
    modelStatuses = {},
    getAgentColor,
    getWorkerDisplayName,
    updateRoleEngine,
    saveRole,
    deleteRole,
    importRole,
    exportRole,
  } = $props<{
    roleTemplates: RoleTemplate[];
    registryAgents: AgentBinding[];
    registryEngines: ModelEngine[];
    inheritModelLabel?: string;
    modelStatuses?: Record<string, { status?: string }>;
    getAgentColor: (templateId: string, colorToken?: string) => { color: string; muted: string };
    getWorkerDisplayName: (workerId: string) => string;
    updateRoleEngine: (templateId: string, engineId: string) => void;
    saveRole: (role: Record<string, unknown>, expectedRoleRevision?: number) => Promise<void>;
    deleteRole: (templateId: string, roleRevision: number) => Promise<void>;
    importRole: (content: string, conflict: 'reject' | 'overwrite' | 'rename', newId?: string) => Promise<void>;
    exportRole: (templateId: string) => Promise<void>;
  }>();

  type RoleStatus = 'bound' | 'inherit' | 'error';

  type RoleAtom = {
    template: RoleTemplate;
    agent: AgentBinding | undefined;
    status: RoleStatus;
    engineId: string;
    displayName: string;
    description: string;
    color: string;
    muted: string;
  };

  type DomainCapability = ProfessionalCapabilitySummary & {
    roleNames: string[];
    availableToAllRoles: boolean;
  };

  function collectDomainCapabilities(roles: RoleAtom[]): DomainCapability[] {
    const capabilities = new Map<string, ProfessionalCapabilitySummary & { roleNames: string[] }>();
    for (const role of roles) {
      for (const capability of role.template.capabilities) {
        const existing = capabilities.get(capability.id);
        if (existing) {
          existing.roleNames.push(role.displayName);
        } else {
          capabilities.set(capability.id, { ...capability, roleNames: [role.displayName] });
        }
      }
    }
    return Array.from(capabilities.values())
      .map((capability) => ({
        ...capability,
        availableToAllRoles: capability.roleNames.length === roles.length,
      }))
      .sort((left, right) => left.id.localeCompare(right.id));
  }

  const selectableEngines = $derived(resolveSelectableRegistryEngines(registryEngines));

  function resolveLocalizedTemplateDisplayName(tmpl: RoleTemplate): string {
    const key = tmpl.i18n?.displayNameKey || `roleTemplate.${tmpl.templateId}.displayName`;
    const translated = i18n.t(key);
    return translated !== key ? translated : tmpl.displayName;
  }

  function formatLocalizedFallbackList(items: string[]): string {
    const normalized = items.map((item) => item.trim()).filter(Boolean);
    if (normalized.length === 0) return '';
    return new Intl.ListFormat(i18n.locale, { style: 'short', type: 'conjunction' }).format(normalized);
  }

  function resolveLocalizedTemplateDescription(tmpl: RoleTemplate): string {
    const key = tmpl.i18n?.descriptionKey || `roleTemplate.${tmpl.templateId}.description`;
    const translated = i18n.t(key);
    if (translated !== key) return translated;
    if (tmpl.description) return tmpl.description;
    const localizedFocus = tmpl.profile.focus.map((item, index) => (
      resolveLocalizedListPhrase(tmpl, 'focus', index, item)
    ));
    return formatLocalizedFallbackList(localizedFocus);
  }

  function resolveLocalizedRolePositioning(tmpl: RoleTemplate): string {
    const key = `roleTemplate.${tmpl.templateId}.role`;
    const translated = i18n.t(key);
    return translated !== key ? translated : tmpl.profile.role;
  }

  function resolveLocalizedListPhrase(
    tmpl: RoleTemplate,
    kind: 'focus' | 'constraints' | 'outputPreferences' | 'ownerships',
    index: number,
    raw: string,
  ): string {
    const key = `roleTemplate.${tmpl.templateId}.${kind}.${index}`;
    const translated = i18n.t(key);
    return translated !== key ? translated : raw;
  }

  const atoms = $derived<RoleAtom[]>(
    (roleTemplates as RoleTemplate[]).map((tmpl: RoleTemplate): RoleAtom => {
      // 故意触达 i18n.locale，使 displayName / description 在切换语言时重新求值
      void i18n.locale;
      const agent = (registryAgents as AgentBinding[]).find((a) => a.templateId === tmpl.templateId);
      const isExplicit = Boolean(agent?.engineId);
      const isOperational = agent ? isAgentBindingOperational(agent, registryEngines) : true;
      const status: RoleStatus = !isExplicit ? 'inherit' : isOperational ? 'bound' : 'error';
      const pair = getAgentColor(tmpl.templateId, tmpl.defaultUI?.colorToken);
      return {
        template: tmpl,
        agent,
        status,
        engineId: isExplicit ? (agent!.engineId as string) : '',
        displayName: resolveLocalizedTemplateDisplayName(tmpl),
        description: resolveLocalizedTemplateDescription(tmpl),
        color: pair.color,
        muted: pair.muted,
      };
    })
  );
  const domainCapabilities = $derived(collectDomainCapabilities(atoms));

  let selectedKey = $state<string | null>(null);

  type RoleDraft = {
    id: string;
    displayName: string;
    description: string;
    positioning: string;
    focus: string;
    constraints: string;
    outputPreferences: string;
    ownerships: string;
    insights: string[];
    capabilities: string[];
    systemPrompt: string;
    parallelismLimit: string;
    roleRevision?: number;
  };
  let editorOpen = $state(false);
  let editorMode = $state<'create' | 'edit'>('create');
  let editorError = $state('');
  let editorBusy = $state(false);
  let draft = $state<RoleDraft>(emptyDraft());
  let importInput: HTMLInputElement;

  function emptyDraft(): RoleDraft {
    return {
      id: '',
      displayName: '',
      description: '',
      positioning: '',
      focus: '',
      constraints: '',
      outputPreferences: '',
      ownerships: '',
      insights: ['decision', 'risk'],
      capabilities: ['general_engineering'],
      systemPrompt: '',
      parallelismLimit: '',
    };
  }

  function listText(values: string[]): string {
    return values.join('\n');
  }

  function draftFromTemplate(template: RoleTemplate, copy = false): RoleDraft {
    return {
      id: copy ? `${template.templateId}-copy` : template.templateId,
      displayName: copy ? `${template.displayName} 副本` : template.displayName,
      description: template.description,
      positioning: template.profile.role,
      focus: listText(template.profile.focus),
      constraints: listText(template.profile.constraints),
      outputPreferences: listText(template.profile.outputPreferences ?? []),
      ownerships: listText(template.ownerships),
      insights: [...template.insightPreferences],
      capabilities: template.capabilities.map((capability) => capability.id),
      systemPrompt: template.systemPrompt ?? '',
      parallelismLimit: template.parallelismLimit ? String(template.parallelismLimit) : '',
      roleRevision: copy ? undefined : template.roleRevision,
    };
  }

  function splitLines(value: string): string[] {
    return value.split(/[\n,]/).map((item) => item.trim()).filter(Boolean);
  }

  function openCreateRole() {
    editorMode = 'create';
    editorError = '';
    draft = emptyDraft();
    editorOpen = true;
  }

  function openEditRole(template: RoleTemplate) {
    if (!template.editable) return;
    editorMode = 'edit';
    editorError = '';
    draft = draftFromTemplate(template);
    editorOpen = true;
  }

  function openCopyRole(template: RoleTemplate) {
    editorMode = 'create';
    editorError = '';
    draft = draftFromTemplate(template, true);
    editorOpen = true;
  }

  function closeEditor() {
    if (!editorBusy) editorOpen = false;
  }

  async function submitRole() {
    editorError = '';
    const payload: Record<string, unknown> = {
      id: draft.id.trim(),
      displayName: draft.displayName.trim(),
      description: draft.description.trim(),
      systemPrompt: draft.systemPrompt.trim(),
      supportedKinds: ['local_agent'],
      parallelismLimit: draft.parallelismLimit.trim() ? Number(draft.parallelismLimit.trim()) : null,
      coordinatorMode: false,
      ...(draft.roleRevision === undefined ? {} : { roleRevision: draft.roleRevision }),
      profile: {
        role: draft.positioning.trim(),
        focus: splitLines(draft.focus),
        constraints: splitLines(draft.constraints),
        outputPreferences: splitLines(draft.outputPreferences),
      },
      ownerships: splitLines(draft.ownerships),
      insightPreferences: draft.insights,
      capabilities: draft.capabilities,
      defaultUI: { colorToken: `agent-${draft.id.trim()}`, icon: 'bot' },
    };
    if (!payload.id || !payload.displayName || !payload.systemPrompt) {
      editorError = '请填写角色 ID、显示名称和系统提示词';
      return;
    }
    if (draft.parallelismLimit.trim()) {
      const parallelismLimit = Number(draft.parallelismLimit.trim());
      if (!Number.isSafeInteger(parallelismLimit) || parallelismLimit <= 0) {
        editorError = '并发上限必须是正整数';
        return;
      }
      payload.parallelismLimit = parallelismLimit;
    }
    editorBusy = true;
    try {
      await saveRole(payload, draft.roleRevision);
      editorOpen = false;
    } catch (error) {
      editorError = error instanceof Error ? error.message : '角色保存失败';
    } finally {
      editorBusy = false;
    }
  }

  async function removeSelectedRole(template: RoleTemplate) {
    if (!template.deletable || template.roleRevision === undefined) return;
    if (!window.confirm(`确定删除角色“${template.displayName}”吗？`)) return;
    try {
      await deleteRole(template.templateId, template.roleRevision);
      selectedKey = null;
    } catch {
      // store 层已显示具体错误；事件处理器消费 Promise，避免未处理拒绝。
    }
  }

  function chooseImportFile() {
    importInput?.click();
  }

  async function handleImportFile(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    let content = '';
    try {
      content = await file.text();
      await importRole(content, 'reject');
    } catch (error) {
      if (!(error instanceof AgentApiError) || error.status !== 409) {
        editorError = error instanceof Error ? error.message : '角色导入失败';
        return;
      }
      const message = error instanceof Error ? error.message : '角色导入失败';
      try {
        if (!window.confirm(`${message}\n\n点击“确定”覆盖已有用户角色，点击“取消”尝试另存为。`)) {
          const newId = window.prompt('请输入新的角色 ID（小写字母、数字和连字符）：');
          if (newId?.trim()) await importRole(content, 'rename', newId.trim());
        } else {
          await importRole(content, 'overwrite');
        }
      } catch {
        // store 层已显示具体错误；导入流程在这里消费后续冲突操作的拒绝。
      }
    }
  }

  async function exportSelectedRole(templateId: string) {
    try {
      await exportRole(templateId);
    } catch {
      // store 层已显示具体错误；避免导出失败形成未处理 Promise。
    }
  }

  $effect(() => {
    if (atoms.length === 0) {
      if (selectedKey !== null) selectedKey = null;
      return;
    }
    if (selectedKey === null || !atoms.some((a: RoleAtom) => a.template.templateId === selectedKey)) {
      selectedKey = atoms[0].template.templateId;
    }
  });

  const selected = $derived<RoleAtom | null>(
    atoms.find((a: RoleAtom) => a.template.templateId === selectedKey) ?? null
  );

  function statusTooltip(status: RoleStatus): string {
    if (status === 'bound') return i18n.t('settings.agents.statusBound');
    if (status === 'error') return i18n.t('settings.agents.statusError');
    return i18n.t('settings.agents.statusInherit');
  }

  function insightLabel(kind: 'decision' | 'contract' | 'risk' | 'constraint'): string {
    switch (kind) {
      case 'decision': return i18n.t('settings.agents.insightDecision');
      case 'contract': return i18n.t('settings.agents.insightContract');
      case 'risk': return i18n.t('settings.agents.insightRisk');
      case 'constraint': return i18n.t('settings.agents.insightConstraint');
    }
  }

  function capabilityName(capability: { id: string; displayName: string }): string {
    const key = `capability.${capability.id}.displayName`;
    const translated = i18n.t(key);
    return translated !== key ? translated : capability.displayName;
  }

  function capabilityDescription(capability: { id: string; description: string }): string {
    const key = `capability.${capability.id}.description`;
    const translated = i18n.t(key);
    return translated !== key ? translated : capability.description;
  }

  function domainCapabilityDescription(capability: DomainCapability): string {
    const description = capabilityDescription(capability);
    if (capability.availableToAllRoles) return description;
    return `${description} · ${i18n.t('settings.agents.domainAvailableToRoles', {
      roles: formatLocalizedFallbackList(capability.roleNames),
    })}`;
  }

  function onTabKeydown(event: KeyboardEvent, idx: number) {
    if (
      event.key !== 'ArrowLeft'
      && event.key !== 'ArrowRight'
      && event.key !== 'ArrowUp'
      && event.key !== 'ArrowDown'
      && event.key !== 'Home'
      && event.key !== 'End'
    ) {
      return;
    }
    event.preventDefault();
    let next = idx;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (idx - 1 + atoms.length) % atoms.length;
    else if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (idx + 1) % atoms.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = atoms.length - 1;
    selectedKey = atoms[next].template.templateId;
  }
</script>

<div class="settings-tab-inner scroll-proxy">
  <div class="agents-scroll-panel settings-scroll-panel">
    <div class="agents-toolbar">
      <div>
        <div class="toolbar-title">子代理角色</div>
        <div class="toolbar-description">内置角色和我的角色共用同一套 Worker 调度能力</div>
      </div>
      <div class="toolbar-actions">
        <button type="button" class="toolbar-button" onclick={chooseImportFile}>导入角色</button>
        <button type="button" class="toolbar-button primary" onclick={openCreateRole}>新建角色</button>
        <input bind:this={importInput} class="visually-hidden" type="file" accept=".md,text/markdown" onchange={handleImportFile} />
      </div>
    </div>
    <div class="agents-shell">
      <div class="agents-tabbar" role="tablist" aria-label={i18n.t('settings.agents.listTitle')}>
        <div class="tabbar-track">
          {#each atoms as atom, idx (atom.template.templateId)}
            {@const isSelected = atom.template.templateId === selectedKey}
            <button
              type="button"
              class="role-tab"
              class:active={isSelected}
              role="tab"
              id="agent-tab-{atom.template.templateId}"
              aria-selected={isSelected}
              aria-controls="agent-tabpanel"
              tabindex={isSelected ? 0 : -1}
              onclick={() => (selectedKey = atom.template.templateId)}
              onkeydown={(e) => onTabKeydown(e, idx)}
            >
              <span class="role-tab-avatar" style="background: {atom.muted}; color: {atom.color}" aria-hidden="true">
                <Icon name="bot" size={11} />
              </span>
              <span class="role-tab-copy">
                <span class="role-tab-name">{atom.displayName}</span>
                <span class="role-tab-subtitle">{statusTooltip(atom.status)}</span>
              </span>
              <span
                class="role-tab-status status-{atom.status}"
                title={statusTooltip(atom.status)}
                aria-label={statusTooltip(atom.status)}
              ></span>
            </button>
          {/each}
        </div>
      </div>

      <div class="agents-content">
        <div
          id="agent-tabpanel"
          class="agents-detail"
          role="tabpanel"
          aria-labelledby={selected ? `agent-tab-${selected.template.templateId}` : undefined}
        >
          {#if !selected}
            <div class="detail-empty">{i18n.t('settings.agents.detailEmpty')}</div>
          {:else}
            {@const tmpl = selected.template}
            {@const positioning = resolveLocalizedRolePositioning(tmpl)}

            <div class="detail-layout">
              <div class="detail-primary">
                <header class="detail-header">
                  <div class="detail-avatar" style="background: {selected.muted}; color: {selected.color}">
                    <Icon name="bot" size={18} />
                  </div>
                  <div class="detail-title-stack">
                    <div class="detail-title-row">
                      <span class="detail-title">{selected.displayName}</span>
                      <span class="source-badge source-{tmpl.source ?? 'builtin'}">{tmpl.source === 'user' ? '我的角色' : '系统内置'}</span>
                      <span class="detail-status-pill status-{selected.status}">{statusTooltip(selected.status)}</span>
                    </div>
                    {#if positioning}
                      <div class="detail-kicker">{positioning}</div>
                    {/if}
                    {#if selected.description}
                      <p class="detail-description">{selected.description}</p>
                    {/if}
                    <div class="detail-actions">
                      <button type="button" class="text-button" onclick={() => openCopyRole(tmpl)}>复制</button>
                      <button type="button" class="text-button" onclick={() => exportSelectedRole(tmpl.templateId)}>导出</button>
                      {#if tmpl.editable}
                        <button type="button" class="text-button" onclick={() => openEditRole(tmpl)}>编辑</button>
                      {/if}
                      {#if tmpl.deletable}
                        <button type="button" class="text-button danger" onclick={() => removeSelectedRole(tmpl)}>删除</button>
                      {/if}
                    </div>
                  </div>
                </header>

                <section class="detail-section engine-row">
                  <div class="section-title">{i18n.t('settings.agents.sectionEngine')}</div>
                  <EnginePicker
                    value={selected.engineId}
                    engines={selectableEngines}
                    inheritModelLabel={inheritModelLabel}
                    getDisplayName={getWorkerDisplayName}
                    modelStatuses={modelStatuses}
                    error={selected.status === 'error'}
                    onchange={(engineId: string) => updateRoleEngine(tmpl.templateId, engineId)}
                  />
                  {#if selected.status === 'error'}
                    <div class="binding-hint err">{i18n.t('settings.agents.engineDisabledHint')}</div>
                  {:else if selected.status === 'inherit'}
                    <div class="binding-hint">{i18n.t('settings.agents.inheritOrchestratorHint')}</div>
                  {/if}
                </section>
              </div>

              <div class="detail-masonry">
                {#if tmpl.profile.focus.length > 0}
                  <section class="detail-section">
                    <div class="section-title">{i18n.t('settings.agents.sectionSpecialties')}</div>
                    <ul class="detail-list">
                      {#each tmpl.profile.focus as item, i}
                        <li>{resolveLocalizedListPhrase(tmpl, 'focus', i, item)}</li>
                      {/each}
                    </ul>
                  </section>
                {/if}

                {#if tmpl.profile.constraints.length > 0}
                  <section class="detail-section">
                    <div class="section-title">{i18n.t('settings.agents.sectionConstraints')}</div>
                    <ul class="detail-list">
                      {#each tmpl.profile.constraints as item, i}
                        <li>{resolveLocalizedListPhrase(tmpl, 'constraints', i, item)}</li>
                      {/each}
                    </ul>
                  </section>
                {/if}

                {#if tmpl.profile.outputPreferences && tmpl.profile.outputPreferences.length > 0}
                  <section class="detail-section">
                    <div class="section-title">{i18n.t('settings.agents.sectionOutput')}</div>
                    <ul class="detail-list">
                      {#each tmpl.profile.outputPreferences as item, i}
                        <li>{resolveLocalizedListPhrase(tmpl, 'outputPreferences', i, item)}</li>
                      {/each}
                    </ul>
                  </section>
                {/if}

                {#if tmpl.ownerships.length > 0}
                  <section class="detail-section">
                    <div class="section-title">{i18n.t('settings.agents.sectionOwnerships')}</div>
                    <div class="chip-row">
                      {#each tmpl.ownerships as item, i}
                        <span class="chip">{resolveLocalizedListPhrase(tmpl, 'ownerships', i, item)}</span>
                      {/each}
                    </div>
                  </section>
                {/if}

                {#if tmpl.insightPreferences.length > 0}
                  <section class="detail-section">
                    <div class="section-title">{i18n.t('settings.agents.sectionInsights')}</div>
                    <div class="chip-row">
                      {#each tmpl.insightPreferences as kind}
                        <span class="chip chip-insight chip-insight-{kind}">{insightLabel(kind)}</span>
                      {/each}
                    </div>
                  </section>
                {/if}
              </div>
            </div>
          {/if}
        </div>

        {#if domainCapabilities.length > 0}
          <section class="domain-library" aria-labelledby="agent-domain-library-title">
            <div class="domain-library-heading">
              <div>
                <div id="agent-domain-library-title" class="section-title">
                  {i18n.t('settings.agents.domainLibraryTitle')}
                </div>
                <p class="domain-library-description">{i18n.t('settings.agents.domainLibraryDescription')}</p>
              </div>
              <span class="domain-library-count" aria-label={i18n.t('settings.agents.domainLibraryCount', { count: domainCapabilities.length })}>
                {domainCapabilities.length}
              </span>
            </div>
            <div class="chip-row capability-list">
              {#each domainCapabilities as capability (capability.id)}
                <span class="chip" title={domainCapabilityDescription(capability)}>
                  {capabilityName(capability)}
                </span>
              {/each}
            </div>
          </section>
        {/if}
      </div>
    </div>
  </div>
</div>

{#if editorOpen}
  <div class="role-editor-backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && closeEditor()}>
    <div class="role-editor" role="dialog" tabindex="-1" aria-modal="true" aria-labelledby="role-editor-title">
      <header class="role-editor-header">
        <div>
          <h2 id="role-editor-title">{editorMode === 'create' ? '新建子代理角色' : '编辑子代理角色'}</h2>
          <p>角色会立即注册到 Magi 的 Worker 目录，并默认继承主模型。</p>
        </div>
        <button type="button" class="text-button" onclick={closeEditor}>关闭</button>
      </header>
      <div class="role-editor-grid">
        <label>角色 ID<input bind:value={draft.id} disabled={editorMode === 'edit'} placeholder="例如：data-analyst" /></label>
        <label>显示名称<input bind:value={draft.displayName} placeholder="例如：数据分析师" /></label>
        <label class="wide">角色描述<input bind:value={draft.description} placeholder="说明这个角色解决什么问题" /></label>
        <label class="wide">角色定位<input bind:value={draft.positioning} placeholder="例如：数据分析与验证" /></label>
        <label>专长（每行一项）<textarea bind:value={draft.focus} rows="4"></textarea></label>
        <label>约束（每行一项）<textarea bind:value={draft.constraints} rows="4"></textarea></label>
        <label>输出偏好（每行一项）<textarea bind:value={draft.outputPreferences} rows="4"></textarea></label>
        <label>核心职责（每行一项）<textarea bind:value={draft.ownerships} rows="4"></textarea></label>
        <label class="wide">系统提示词<textarea bind:value={draft.systemPrompt} rows="8" placeholder="描述该 Worker 的职责、工作边界和输出要求"></textarea></label>
        <label>并发上限（可选）<input bind:value={draft.parallelismLimit} inputmode="numeric" placeholder="不填表示不限" /></label>
        <fieldset class="wide">
          <legend>信号偏好</legend>
          <div class="checkbox-grid">
            {#each ['decision', 'contract', 'risk', 'constraint'] as insight}
              <label class="checkbox-label"><input type="checkbox" checked={draft.insights.includes(insight)} onchange={() => (draft.insights = draft.insights.includes(insight) ? draft.insights.filter((item) => item !== insight) : [...draft.insights, insight])} />{insightLabel(insight as 'decision' | 'contract' | 'risk' | 'constraint')}</label>
            {/each}
          </div>
        </fieldset>
        <fieldset class="wide">
          <legend>可用专业能力</legend>
          <div class="checkbox-grid capability-editor-grid">
            {#each domainCapabilities as capability (capability.id)}
              <label class="checkbox-label" title={capabilityDescription(capability)}><input type="checkbox" checked={draft.capabilities.includes(capability.id)} onchange={() => (draft.capabilities = draft.capabilities.includes(capability.id) ? draft.capabilities.filter((item) => item !== capability.id) : [...draft.capabilities, capability.id])} />{capabilityName(capability)}</label>
            {/each}
          </div>
        </fieldset>
      </div>
      {#if editorError}<div class="role-editor-error">{editorError}</div>{/if}
      <footer class="role-editor-footer">
        <button type="button" class="toolbar-button" onclick={closeEditor} disabled={editorBusy}>取消</button>
        <button type="button" class="toolbar-button primary" onclick={() => void submitRole()} disabled={editorBusy}>{editorBusy ? '保存中…' : '保存角色'}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .settings-tab-inner {
    container-type: inline-size;
    container-name: agents-tab;
    /* 覆盖 settings.css 默认值：本 tab 自己用 .settings-scroll-panel 承担滚动 */
    height: 100%;
    width: 100%;
    overflow: hidden;
  }

  .scroll-proxy { min-height: 0; }
  .settings-scroll-panel {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 20px 4px;
    scrollbar-width: none;
    display: flex;
    flex-direction: column;
  }
  .settings-scroll-panel::-webkit-scrollbar { width: 0; }

  .agents-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 14px 0 12px;
    border-bottom: 1px solid var(--ind-border-separator);
    margin-bottom: 2px;
  }
  .toolbar-title { font-size: 14px; font-weight: 650; color: var(--ind-foreground); }
  .toolbar-description { margin-top: 4px; color: var(--ind-foreground-soft); font-size: 12px; }
  .toolbar-actions, .detail-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .toolbar-button {
    border: 1px solid var(--ind-border-separator);
    background: var(--ind-bg-control);
    color: var(--ind-foreground-secondary);
    border-radius: 7px;
    padding: 7px 12px;
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .toolbar-button:hover { background: var(--ind-bg-control-hover); color: var(--ind-foreground); }
  .toolbar-button.primary { background: var(--ind-tab-accent); border-color: var(--ind-tab-accent); color: white; }
  .toolbar-button:disabled { opacity: .55; cursor: default; }
  .text-button {
    border: 0;
    padding: 0;
    background: transparent;
    color: var(--ind-tab-accent);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .text-button.danger { color: var(--error, #ff3b30); }
  .source-badge {
    display: inline-flex;
    align-items: center;
    border-radius: 999px;
    padding: 3px 7px;
    font-size: 10px;
    font-weight: 600;
  }
  .source-badge.source-builtin { color: var(--ind-foreground-soft); background: color-mix(in srgb, var(--ind-foreground) 7%, transparent); }
  .source-badge.source-user { color: var(--ind-tab-accent); background: color-mix(in srgb, var(--ind-tab-accent) 10%, transparent); }
  .detail-actions { margin-top: 10px; }
  .visually-hidden { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }

  .role-editor-backdrop {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    background: color-mix(in srgb, #000 42%, transparent);
  }
  .role-editor {
    width: min(760px, 100%);
    max-height: min(820px, 94vh);
    overflow: auto;
    border: 1px solid var(--ind-border-separator);
    border-radius: 12px;
    background: var(--ind-bg-primary, #fff);
    box-shadow: 0 24px 70px rgb(0 0 0 / 22%);
    padding: 22px;
  }
  .role-editor-header, .role-editor-footer { display: flex; align-items: flex-start; justify-content: space-between; gap: 14px; }
  .role-editor-header h2 { margin: 0; font-size: 18px; color: var(--ind-foreground); }
  .role-editor-header p { margin: 7px 0 0; color: var(--ind-foreground-soft); font-size: 12px; }
  .role-editor-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; margin-top: 20px; }
  .role-editor-grid label, .role-editor-grid legend { color: var(--ind-foreground-secondary); font-size: 12px; font-weight: 600; }
  .role-editor-grid input, .role-editor-grid textarea { display: block; width: 100%; box-sizing: border-box; margin-top: 6px; border: 1px solid var(--ind-border-separator); border-radius: 7px; background: var(--ind-bg-control); color: var(--ind-foreground); padding: 8px 9px; font: inherit; font-size: 13px; resize: vertical; }
  .role-editor-grid input:focus, .role-editor-grid textarea:focus { outline: 2px solid color-mix(in srgb, var(--ind-tab-accent) 34%, transparent); outline-offset: 1px; }
  .role-editor-grid .wide { grid-column: 1 / -1; }
  .role-editor-grid fieldset { min-width: 0; border: 1px solid var(--ind-border-separator); border-radius: 8px; padding: 12px; }
  .checkbox-grid { display: flex; flex-wrap: wrap; gap: 9px 16px; margin-top: 8px; }
  .checkbox-label { display: inline-flex !important; align-items: center; gap: 6px; font-weight: 500 !important; cursor: pointer; }
  .checkbox-label input { width: auto; margin: 0; }
  .role-editor-error { margin-top: 14px; padding: 9px 11px; border-radius: 7px; color: var(--error, #ff3b30); background: color-mix(in srgb, var(--error, #ff3b30) 9%, transparent); font-size: 12px; }
  .role-editor-footer { align-items: center; justify-content: flex-end; margin-top: 18px; }

  .agents-shell {
    display: grid;
    grid-template-columns: 220px minmax(0, 1fr);
    gap: 22px;
    min-height: 100%;
    align-items: stretch;
  }

  .agents-tabbar {
    position: relative;
    overflow: hidden;
    scrollbar-width: none;
    border-right: 1px solid var(--ind-border-separator);
    padding: 8px 14px 8px 0;
  }
  .agents-tabbar::-webkit-scrollbar { height: 0; }
  .tabbar-track {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }

  .role-tab {
    position: relative;
    display: grid;
    grid-template-columns: 28px minmax(0, 1fr) 7px;
    align-items: center;
    gap: 10px;
    width: 100%;
    min-height: 48px;
    padding: 7px 9px;
    border: none;
    border-radius: 8px;
    background: transparent;
    color: var(--ind-foreground-muted);
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    letter-spacing: -0.005em;
    cursor: pointer;
    transition: background 0.15s ease, color 0.15s ease;
    text-align: left;
  }
  .role-tab:hover {
    background: var(--ind-bg-control);
    color: var(--ind-foreground-secondary);
  }
  .role-tab.active {
    background: var(--ind-bg-control-hover);
    color: var(--ind-foreground);
    font-weight: 600;
  }
  .role-tab.active::before {
    content: '';
    position: absolute;
    left: 0;
    top: 9px;
    bottom: 9px;
    width: 2px;
    background: var(--ind-tab-accent);
    border-radius: 2px;
  }
  .role-tab:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--ind-tab-accent) 60%, transparent);
    outline-offset: -3px;
    border-radius: 4px;
  }

  .role-tab-avatar {
    width: 28px; height: 28px;
    border-radius: 8px;
    display: inline-flex; align-items: center; justify-content: center;
    flex-shrink: 0;
    opacity: 0.78;
    transition: opacity 0.15s ease;
  }
  .role-tab.active .role-tab-avatar { opacity: 1; }
  .role-tab:hover .role-tab-avatar { opacity: 0.9; }

  .role-tab-copy {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .role-tab-name {
    font-variant-numeric: tabular-nums;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .role-tab-subtitle {
    font-size: 11.5px;
    font-weight: 500;
    line-height: 1.2;
    color: var(--ind-foreground-soft);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .role-tab-status {
    width: 6px; height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .role-tab-status.status-bound { background: var(--success, #34c759); }
  .role-tab-status.status-inherit { background: color-mix(in srgb, var(--ind-foreground-soft) 55%, transparent); }
  .role-tab-status.status-error { background: var(--error, #ff3b30); }

  /* ---------- Detail Panel ---------- */
  .agents-detail {
    display: flex;
    min-height: 0;
    min-width: 0;
    padding: 8px 0 16px;
  }

  .agents-content {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .domain-library {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-width: 0;
    padding: 20px 0 24px;
    border-top: 1px solid var(--ind-border-separator);
  }

  .domain-library-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }

  .domain-library-description {
    margin: 5px 0 0;
    font-size: 12.5px;
    line-height: 1.55;
    color: var(--ind-foreground-muted);
  }

  .domain-library-count {
    flex: 0 0 auto;
    min-width: 28px;
    height: 22px;
    padding: 0 7px;
    border-radius: 7px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 11.5px;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
    color: var(--ind-tab-accent);
    background: color-mix(in srgb, var(--ind-tab-accent) 10%, var(--ind-bg-control));
    border: 1px solid color-mix(in srgb, var(--ind-tab-accent) 22%, var(--ind-border-control));
  }

  .domain-library .section-title {
    color: var(--ind-tab-accent);
  }

  .domain-library .capability-list .chip {
    color: var(--ind-tab-accent);
    background: color-mix(in srgb, var(--ind-tab-accent) 8%, transparent);
    border-color: color-mix(in srgb, var(--ind-tab-accent) 24%, var(--ind-border-separator));
  }

  .detail-empty {
    color: var(--ind-foreground-muted);
    font-size: 13px;
    text-align: center;
    padding: 32px 0;
  }

  .detail-layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 22px;
    align-items: start;
    align-content: start;
    width: 100%;
    min-width: 0;
  }

  .detail-primary {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(240px, 300px);
    gap: 24px;
    align-items: start;
    min-width: 0;
    padding-bottom: 18px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .detail-header {
    display: grid;
    grid-template-columns: 44px minmax(0, 1fr);
    gap: 14px;
    align-items: start;
  }
  .detail-avatar {
    width: 44px; height: 44px;
    border-radius: 12px;
    display: flex; align-items: center; justify-content: center;
    flex-shrink: 0;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 12%, transparent);
  }
  .detail-title-row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }

  .engine-row {
    gap: 9px;
    padding: 0;
    margin: 0;
    border-bottom: none;
  }

  .detail-masonry {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    column-gap: 34px;
    row-gap: 0;
    align-items: start;
    min-width: 0;
  }
  .detail-title-stack {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 6px;
  }
  .detail-title {
    font-size: 18px;
    font-weight: 650;
    color: var(--ind-foreground);
    letter-spacing: -0.018em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .detail-status-pill {
    display: inline-flex;
    align-items: center;
    height: 22px;
    padding: 0 8px;
    border-radius: 999px;
    border: 1px solid var(--ind-border-control);
    font-size: 11.5px;
    font-weight: 600;
    line-height: 1;
    color: var(--ind-foreground-secondary);
    background: var(--ind-bg-control);
    white-space: nowrap;
  }
  .detail-status-pill.status-bound {
    color: var(--success, #34c759);
    border-color: color-mix(in srgb, var(--success, #34c759) 28%, var(--ind-border-control));
    background: color-mix(in srgb, var(--success, #34c759) 8%, var(--ind-bg-control));
  }
  .detail-status-pill.status-inherit {
    color: var(--ind-foreground-muted);
  }
  .detail-status-pill.status-error {
    color: var(--error, #ff3b30);
    border-color: color-mix(in srgb, var(--error, #ff3b30) 34%, var(--ind-border-control));
    background: color-mix(in srgb, var(--error, #ff3b30) 8%, var(--ind-bg-control));
  }
  .detail-kicker {
    font-size: 13px;
    font-weight: 500;
    color: var(--ind-foreground-muted);
    letter-spacing: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .detail-description {
    font-size: 13.5px;
    line-height: 1.58;
    color: var(--ind-foreground-secondary);
    margin: 0;
    text-wrap: pretty;
  }

  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 9px;
    min-width: 0;
    padding: 0 0 18px;
    margin: 0 0 18px;
    border-bottom: 1px solid var(--ind-border-separator);
  }
  .detail-masonry .detail-section:nth-last-child(-n + 2) {
    margin-bottom: 0;
    border-bottom: none;
  }

  /* 与 SettingsModelTab.settings-section-title 对齐：14px / 700，常规文字色（非 uppercase microcopy） */
  .section-title {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0;
    text-transform: none;
    color: var(--ind-foreground);
  }

  /* Engine binding hint */
  .binding-hint {
    font-size: 12px;
    line-height: 1.55;
    color: var(--ind-foreground-muted);
    padding-left: 2px;
  }
  .binding-hint.err { color: var(--error, #ff3b30); }

  /* Lists */
  .detail-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .detail-list li {
    position: relative;
    padding-left: 16px;
    font-size: 13px;
    line-height: 1.55;
    color: var(--ind-foreground-secondary);
    text-wrap: pretty;
  }
  .detail-list li::before {
    content: '';
    position: absolute;
    left: 4px;
    top: 10px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--ind-foreground-soft);
  }

  /* Chips */
  .chip-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    height: 28px;
    padding: 0 12px;
    border-radius: 7px;
    font-size: 12.5px;
    font-weight: 500;
    color: var(--ind-foreground-secondary);
    background: color-mix(in srgb, var(--ind-foreground) 5%, transparent);
    border: 1px solid var(--ind-border-separator);
    letter-spacing: -0.005em;
  }
  .chip-insight-decision { color: var(--ind-tab-accent); border-color: color-mix(in srgb, var(--ind-tab-accent) 24%, transparent); background: color-mix(in srgb, var(--ind-tab-accent) 8%, transparent); }
  .chip-insight-contract { color: #5856d6; border-color: color-mix(in srgb, #5856d6 24%, transparent); background: color-mix(in srgb, #5856d6 8%, transparent); }
  .chip-insight-risk { color: var(--error, #ff3b30); border-color: color-mix(in srgb, var(--error, #ff3b30) 24%, transparent); background: color-mix(in srgb, var(--error, #ff3b30) 8%, transparent); }
  .chip-insight-constraint { color: var(--warning, #ff9500); border-color: color-mix(in srgb, var(--warning, #ff9500) 28%, transparent); background: color-mix(in srgb, var(--warning, #ff9500) 9%, transparent); }

  /* ---------- Responsive ---------- */
  @container agents-tab (max-width: 760px) {
    .agents-shell {
      grid-template-columns: 1fr;
      gap: 16px;
    }
    .agents-tabbar {
      border-right: none;
      border-bottom: 1px solid var(--ind-border-separator);
      padding: 0 0 10px;
      overflow-x: auto;
    }
    .tabbar-track {
      flex-direction: row;
      min-width: max-content;
      gap: 4px;
    }
    .role-tab {
      flex: 0 0 auto;
      width: auto;
      min-height: 38px;
      grid-template-columns: 20px max-content 6px;
      padding: 7px 10px;
      white-space: nowrap;
    }
    .role-tab-copy {
      min-width: max-content;
    }
    .role-tab-name {
      overflow: visible;
      text-overflow: clip;
    }
    .role-tab.active::before {
      left: 10px;
      right: 10px;
      top: auto;
      bottom: -10px;
      width: auto;
      height: 2px;
    }
    .role-tab-subtitle {
      display: none;
    }
    .role-tab-avatar {
      width: 20px;
      height: 20px;
      border-radius: 6px;
    }
    .detail-layout {
      grid-template-columns: 1fr;
      gap: 22px;
    }
    .detail-primary {
      grid-template-columns: 1fr;
      gap: 18px;
    }
    .engine-row {
      padding-top: 16px;
      border-top: 1px solid var(--ind-border-separator);
    }
  }

  @container agents-tab (max-width: 560px) {
    .agents-toolbar { align-items: flex-start; flex-direction: column; }
    .toolbar-actions { width: 100%; }
    .toolbar-button { flex: 1; }
    .role-editor-grid { grid-template-columns: 1fr; }
    .role-editor-grid .wide { grid-column: auto; }
    .detail-masonry {
      grid-template-columns: 1fr;
    }
    .detail-masonry .detail-section {
      margin-bottom: 18px;
      border-bottom: 1px solid var(--ind-border-separator);
    }
    .detail-masonry .detail-section:last-child {
      margin-bottom: 0;
      border-bottom: none;
    }
    .role-tab {
      grid-template-columns: max-content 6px;
      padding: 9px 10px 8px;
    }
    .role-tab-avatar {
      display: none;
    }
  }
</style>

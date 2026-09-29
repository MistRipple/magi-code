<script lang="ts">
import '../styles/settings.css';
import SettingsStatsTab from './SettingsStatsTab.svelte';
import SettingsRulesTab from './SettingsRulesTab.svelte';
import SettingsAgentsTab from './SettingsAgentsTab.svelte';
import SettingsModelTab from './SettingsModelTab.svelte';
import SettingsToolsTab from './SettingsToolsTab.svelte';
import SettingsBrowserTab from './SettingsBrowserTab.svelte';
import SettingsProjectTab from './SettingsProjectTab.svelte';
import SettingsAppearanceTab from './SettingsAppearanceTab.svelte';
import { tick, untrack } from 'svelte';
import Icon from './Icon.svelte';
import Modal from './Modal.svelte';
import Toggle from './Toggle.svelte';
import { i18n } from '../stores/i18n.svelte';
import {
    updateAgentRuntimeSetting,
  } from '../web/agent-api';
import WebFolderPicker from '../web/WebFolderPicker.svelte';
import { getAgentColor } from '../lib/agent-colors';
import { SETTINGS_TABS } from '../lib/settings-tabs';

  import { useSettingsStore } from '../stores/settings-store.svelte';
  
  interface Props {
    /** 当前是否显示。设置与工作台是同级视图：不显示时保活，只是不参与布局。 */
    active: boolean;
    onClose?: () => void;
  }
  
  let { active, onClose }: Props = $props();

  const store = useSettingsStore({
    onClose: () => onClose?.(),
    // 保活期间不可见时，存储不做统计这类重活（见 settings-store 中的说明）。
    isActive: () => active,
  });

  const activeTabDefinition = $derived(
    SETTINGS_TABS.find((tab) => tab.id === store.activeTab) ?? SETTINGS_TABS[0],
  );

  // 返回要先等待保存队列与清理完成（见 store.closeSettings）；期间禁用，避免重复触发。
  let leaving = $state(false);
  async function goBack(): Promise<void> {
    if (leaving) return;
    leaving = true;
    try {
      await store.closeSettings();
    } finally {
      leaving = false;
    }
  }

  function isTextEntry(target: EventTarget | null): boolean {
    return target instanceof HTMLElement
      && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName));
  }

  // Esc = 返回，但要把 Esc 让给更内层的东西：嵌套弹窗（自行拦截）、打开的下拉列表、正在输入的控件。
  function handleWindowKeydown(event: KeyboardEvent): void {
    if (!active || event.key !== 'Escape' || event.defaultPrevented) return;
    if (Object.values(store.modelDropdownOpen).some(Boolean)) {
      store.closeAllModelDropdowns();
      event.preventDefault();
      return;
    }
    if (isTextEntry(event.target)) return;
    event.preventDefault();
    void goBack();
  }

  // 每次重新显示：刷新只读数据，并把焦点放到当前分类，键盘用户可以直接继续。
  // 首次创建时（预挂载在后台）不抢焦点。
  let navElement = $state<HTMLElement | null>(null);
  let wasActive = false;
  $effect(() => {
    const nowActive = active;
    if (nowActive && !wasActive) {
      untrack(() => {
        store.refreshOnShow();
      });
      void tick().then(() => {
        navElement?.querySelector<HTMLElement>('[aria-current="page"]')?.focus({ preventScroll: true });
      });
    }
    wasActive = nowActive;
  });

</script>


<svelte:window onkeydown={handleWindowKeydown} />

<!-- 设置与工作台是同级视图：这里不是叠加层。不显示时 display:none 保活（分类、滚动、未保存内容都保留）。 -->
<div
  class="settings-page"
  class:settings-page--away={!active}
  inert={!active}
  role="region"
  aria-label={i18n.t('settings.title')}
  data-testid="settings-page"
>
  <!-- 左侧：设置导航（与侧栏同宽、同材质，视觉上像侧栏切换成了设置导航） -->
  <aside class="settings-nav" bind:this={navElement}>
    <button
      type="button"
      class="settings-back"
      data-testid="settings-back"
      disabled={leaving}
      onclick={() => void goBack()}
    >
      <Icon name="chevron-left" size={14} />
      <span>{i18n.t('settings.back')}</span>
    </button>
    <div class="settings-nav-caption" aria-hidden="true">{i18n.t('settings.title')}</div>
    <nav class="settings-nav-list" aria-label={i18n.t('settings.title')}>
      {#each SETTINGS_TABS as tab (tab.id)}
        <button
          type="button"
          class="settings-nav-item"
          class:active={store.activeTab === tab.id}
          aria-current={store.activeTab === tab.id ? 'page' : undefined}
          data-testid={`settings-nav-${tab.id}`}
          onclick={() => store.activeTab = tab.id}
        >
          <Icon name={tab.icon} size={15} />
          <span>{i18n.t(tab.titleKey)}</span>
        </button>
      {/each}
    </nav>
    {#if store.userInfo && store.clientKind === 'vscode'}
      <div class="settings-nav-footer">
        <span class="user-info-text" title={store.userInfo}>{store.userInfo}</span>
        <button class="btn btn--secondary btn--sm" onclick={store.logout}>{i18n.t('settings.logout')}</button>
      </div>
    {/if}
  </aside>

  <!-- 右侧：当前分类的页面 -->
  <main class="settings-main">
    <header class="main-header">
      <div class="header-breadcrumbs">
        <div class="header-title-group">
          <h2>{i18n.t(activeTabDefinition.titleKey)}</h2>
          <span class="header-description">{i18n.t(activeTabDefinition.descKey)}</span>
        </div>
      </div>
      <div class="header-actions">
        <div class="locale-selector">
          <button
            class="locale-btn"
            class:active={i18n.locale === 'zh-CN'}
            onclick={async () => {
              i18n.setLocale('zh-CN');
              await updateAgentRuntimeSetting('locale', 'zh-CN');
              await store.reloadRoleTemplates();
            }}
          >
            {i18n.t('settings.locale.zhCN')}
          </button>
          <button
            class="locale-btn"
            class:active={i18n.locale === 'en-US'}
            onclick={async () => {
              i18n.setLocale('en-US');
              await updateAgentRuntimeSetting('locale', 'en-US');
              await store.reloadRoleTemplates();
            }}
          >
            {i18n.t('settings.locale.enUS')}
          </button>
        </div>
      </div>
    </header>

      <!-- Tab 内容区域 -->
      <div class="settings-tab-content scroll-content" onscroll={() => { store.closeAllModelDropdowns(); }}>
      {#if store.activeTab === 'appearance'}
        <SettingsAppearanceTab />
      {:else if store.activeTab === 'project'}
        <SettingsProjectTab />
      {:else if store.activeTab === 'stats'}
        <!-- 统计 Tab -->
        <SettingsStatsTab totalInputTokens={store.totalInputTokens} totalOutputTokens={store.totalOutputTokens} totalTokens={store.totalTokens} isRefreshing={store.isRefreshing} refreshConnections={store.refreshConnections} showResetConfirmDialog={store.showResetConfirmDialog} modelStatuses={store.modelStatuses} bindingUsageStats={store.executionStats} modelUsageStats={store.executionModelStats} statsDisplayKeys={store.statsDisplayKeys} getWorkerStats={store.getWorkerStats} getStatsRoleModelStatus={store.getStatsRoleModelStatus} getStatusClass={store.getStatusClass} getWorkerDisplayName={store.getWorkerDisplayName} statusTexts={store.statusTexts}
        />
      {:else if store.activeTab === 'model'}
        <!-- 模型配置 Tab -->
        <SettingsModelTab bind:modelConfigTab={store.modelConfigTab} bind:orchConfig={store.orchConfig} bind:compConfig={store.compConfig} bind:visionConfig={store.visionConfig} visionBuiltinTextModelRules={store.visionBuiltinTextModelRules} bind:imageConfig={store.imageConfig} bind:workerConfigs={store.workerConfigs} modelConfigBaselines={store.modelConfigBaselines} workerModelTabs={store.workerModelTabs} modelStatuses={store.modelStatuses} saveStatus={store.saveStatus} testStatus={store.testStatus} fetchingModels={store.fetchingModels} bind:keyVisible={store.keyVisible} modelDropdownOpen={store.modelDropdownOpen} dropdownPosition={store.dropdownPosition} modelLists={store.modelLists} roleTemplates={store.roleTemplates} registryAgents={store.registryAgents} getBaseUrlPlaceholder={store.getBaseUrlPlaceholder} shouldRecommendStandardUrlMode={store.shouldRecommendStandardUrlMode} openModelDropdown={store.openModelDropdown} closeModelDropdown={store.closeModelDropdown} fetchModelList={store.fetchModelList} selectModel={store.selectModel} saveModelConfig={store.saveModelConfig} testModelConnection={store.testModelConnection} getStatusClass={store.getStatusClass} getStatusText={store.getStatusText} getWorkerDisplayName={store.getWorkerDisplayName}
          {getAgentColor} deleteEngine={store.deleteEngine} openAddEngineDialog={store.openAddEngineDialog} renameEngineDisplay={store.renameEngineDisplay}
        />
      {:else if store.activeTab === 'agents'}
        <!-- 角色管理 Tab -->
        <SettingsAgentsTab roleTemplates={store.roleTemplates} registryAgents={store.registryAgents} registryEngines={store.registryEngines}
          inheritModelLabel={store.orchConfig.model ?? ''} modelStatuses={store.modelStatuses}
          {getAgentColor} getWorkerDisplayName={store.getWorkerDisplayName} updateRoleEngine={store.updateRoleEngine}
          saveRole={store.saveAgentRoleDefinition} deleteRole={store.removeAgentRoleDefinition}
          importRole={store.importAgentRoleDefinition} exportRole={store.exportAgentRoleDefinition}
        />
      {:else if store.activeTab === 'rules'}
        <!-- 规则 Tab -->
        <SettingsRulesTab bind:userRules={store.userRules} bind:newCustomRule={store.newCustomRule} SAFEGUARD_CATEGORIES={store.SAFEGUARD_CATEGORIES} getRulesForCategory={store.getRulesForCategory} toggleSafeguardRule={store.toggleSafeguardRule} updateSafeguardRuleAction={store.updateSafeguardRuleAction} removeCustomRule={store.removeCustomRule} addCustomRule={store.addCustomRule} userRulesSaveStatus={store.userRulesSaveStatus} safeguardSaveStatus={store.safeguardSaveStatus} safeguardAuditCount={store.safeguardAuditCount} safeguardAuditPersistenceHealthy={store.safeguardAuditPersistenceHealthy} conversationDisplayMode={store.conversationDisplayMode} conversationDisplaySaveStatus={store.conversationDisplaySaveStatus} saveConversationDisplayMode={store.saveConversationDisplayMode}
        />
      {:else if store.activeTab === 'tools'}
        <!-- 工具 Tab -->
        <SettingsToolsTab mcpServersHydrated={store.mcpServersHydrated} mcpServersLoading={store.mcpServersLoading} mcpServers={store.mcpServers} mcpExpandedServer={store.mcpExpandedServer} mcpServerTools={store.mcpServerTools} mcpRefreshingServers={store.mcpRefreshingServers} builtinTools={store.builtinTools} builtinToolsLoading={store.builtinToolsLoading} capabilityDependencies={store.capabilityDependencies} commandEnvironment={store.commandEnvironment} commandEnvironmentLoading={store.commandEnvironmentLoading} skills={store.skills} skillUpdateAvailableCount={store.skillUpdateAvailableCount} skillUpdatesChecking={store.skillUpdatesChecking} skillUpdatingIds={store.skillUpdatingIds} skillTogglingIds={store.skillTogglingIds} openMCPDialog={store.openMCPDialog} toggleMCPExpand={store.toggleMCPExpand} getMCPHealthLabel={store.getMCPHealthLabel} toggleMCPServer={store.toggleMCPServer} deleteMCPServer={store.deleteMCPServer} refreshMCPTools={store.refreshMCPTools} refreshBuiltinToolCatalog={store.refreshBuiltinToolCatalog} refreshCommandEnvironment={store.refreshCommandEnvironment} openSkillLibraryDialog={store.openSkillLibraryDialog} openRepoDialog={store.openRepoDialog} checkSkillUpdates={store.checkSkillUpdates} updateSkill={store.updateSkill} toggleSkill={store.toggleSkill} updateAllSkills={store.updateAllSkills} rollbackSkill={store.rollbackSkill} deleteSkill={store.deleteSkill}
        />
      {:else if store.activeTab === 'browser'}
        <SettingsBrowserTab />
      {/if}
    </div>
  </main>
</div>
<!-- 输入对话框 -->
{#if store.showInputDialog}
  <Modal
    size="sm"
    closeOnEscape={true}
    onClose={store.cancelInputDialog}
  >
    {#snippet header()}
      <h3>{store.inputDialogTitle}</h3>
    {/snippet}
    <div class="form-field">
      <input class="form-input" type="text" bind:value={store.inputDialogValue} placeholder={i18n.t('settings.dialog.inputPlaceholder')}>
    </div>
    {#snippet footer()}
      <button class="btn btn--secondary btn--sm" onclick={store.cancelInputDialog}>{i18n.t('settings.dialog.cancel')}</button>
      <button class="btn btn--primary btn--sm" onclick={store.confirmInputDialog}>{i18n.t('settings.dialog.confirm')}</button>
    {/snippet}
  </Modal>
{/if}

<!-- MCP 对话框 -->
{#if store.showMCPDialogState}
  <Modal
    size="lg"
    closeOnEscape={true}
    onClose={store.closeMCPDialog}
  >
    {#snippet header()}
      <div class="mcp-editor-title">
        <h3>{store.mcpDialogIsEdit ? i18n.t('settings.mcp.editTitle') : i18n.t('settings.mcp.addTitle')}</h3>
        <span>{i18n.t('settings.mcp.editorDesc')}</span>
      </div>
      <button class="modal-close" type="button" onclick={store.closeMCPDialog} aria-label={i18n.t('settings.closeSettings')} title={i18n.t('settings.closeSettings')}>
        <Icon name="close" size={18} />
      </button>
    {/snippet}
    <div class="mcp-editor">
      <div class="mcp-mode-switch" role="tablist" aria-label={i18n.t('settings.mcp.addMode')}>
        <button
          type="button"
          role="tab"
          aria-selected={store.mcpDialogMode === 'form'}
          class:active={store.mcpDialogMode === 'form'}
          onclick={() => store.setMcpDialogMode('form')}
        >
          {i18n.t('settings.mcp.formMode')}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={store.mcpDialogMode === 'json'}
          class:active={store.mcpDialogMode === 'json'}
          onclick={() => store.setMcpDialogMode('json')}
        >
          {i18n.t('settings.mcp.jsonMode')}
        </button>
      </div>

      {#if store.mcpDialogMode === 'form'}
        <div class="mcp-form">
          <div class="mcp-form-grid">
            <div class="form-field">
              <label class="form-label" for="mcp-name">{i18n.t('settings.mcp.nameLabel')}</label>
              <input
                class="form-input"
                id="mcp-name"
                type="text"
                value={store.mcpFormDraft.name}
                placeholder={i18n.t('settings.mcp.namePlaceholder')}
                oninput={(event) => store.updateMcpFormField('name', event.currentTarget.value)}
              >
            </div>
            <div class="form-field">
              <span class="form-label mcp-field-label">{i18n.t('settings.mcp.transportLabel')}</span>
              <div class="ui-segmented mcp-transport-switch">
                <button
                  class="ui-segmented__option"
                  type="button"
                  class:active={store.mcpFormDraft.type === 'stdio'}
                  onclick={() => store.updateMcpFormField('type', 'stdio')}
                >{i18n.t('settings.mcp.transportStdio')}</button>
                <button
                  class="ui-segmented__option"
                  type="button"
                  class:active={store.mcpFormDraft.type === 'streamable-http'}
                  onclick={() => store.updateMcpFormField('type', 'streamable-http')}
                >{i18n.t('settings.mcp.transportHttp')}</button>
              </div>
            </div>
          </div>

          <div class="mcp-form-options">
            <div class="mcp-timeout-field">
              <label class="form-label" for="mcp-timeout">{i18n.t('settings.mcp.timeoutLabel')}</label>
              <div class="mcp-input-suffix">
                <input
                  class="form-input"
                  id="mcp-timeout"
                  type="number"
                  min="1"
                  max="300"
                  step="1"
                  value={store.mcpFormDraft.requestTimeoutSeconds}
                  placeholder="30"
                  oninput={(event) => store.updateMcpFormField('requestTimeoutSeconds', event.currentTarget.value)}
                >
                <span>{i18n.t('settings.mcp.seconds')}</span>
              </div>
            </div>
            <div class="mcp-enabled-field">
              <div>
                <strong>{i18n.t('settings.mcp.enabledLabel')}</strong>
                <span>{i18n.t('settings.mcp.enabledHint')}</span>
              </div>
              <Toggle
                checked={store.mcpFormDraft.enabled}
                onchange={(checked) => store.updateMcpFormField('enabled', checked)}
              />
            </div>
          </div>

          {#if store.mcpFormDraft.type === 'stdio'}
            <section class="mcp-form-section">
              <div class="mcp-section-heading">
                <div>
                  <strong>{i18n.t('settings.mcp.stdioSection')}</strong>
                  <span>{i18n.t('settings.mcp.stdioSectionDesc')}</span>
                </div>
              </div>
              <div class="form-field">
                <label class="form-label" for="mcp-command">{i18n.t('settings.mcp.commandLabel')}</label>
                <input
                  class="form-input"
                  id="mcp-command"
                  type="text"
                  value={store.mcpFormDraft.command}
                  placeholder={i18n.t('settings.mcp.commandPlaceholder')}
                  oninput={(event) => store.updateMcpFormField('command', event.currentTarget.value)}
                >
              </div>
              <div class="mcp-list-field">
                <div class="mcp-list-heading">
                  <span>{i18n.t('settings.mcp.argsLabel')}</span>
                  <button type="button" onclick={store.addMcpFormArg}>
                    <Icon name="plus" size={13} />
                    {i18n.t('settings.mcp.addArg')}
                  </button>
                </div>
                {#if store.mcpFormDraft.args.length === 0}
                  <div class="mcp-empty-row">{i18n.t('settings.mcp.noArgs')}</div>
                {:else}
                  <div class="mcp-row-list">
                    {#each store.mcpFormDraft.args as arg, index}
                      <div class="mcp-value-row">
                        <input
                          class="form-input"
                          type="text"
                          value={arg}
                          aria-label={i18n.t('settings.mcp.argIndex', { index: index + 1 })}
                          placeholder={i18n.t('settings.mcp.argPlaceholder')}
                          oninput={(event) => store.updateMcpFormArg(index, event.currentTarget.value)}
                        >
                        <button type="button" class="mcp-remove-row" title={i18n.t('settings.mcp.removeRow')} onclick={() => store.removeMcpFormArg(index)}>
                          <Icon name="close" size={14} />
                        </button>
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
              <div class="mcp-list-field">
                <div class="mcp-list-heading">
                  <span>{i18n.t('settings.mcp.envLabel')}</span>
                  <button type="button" onclick={() => store.addMcpFormKeyValue('env')}>
                    <Icon name="plus" size={13} />
                    {i18n.t('settings.mcp.addVariable')}
                  </button>
                </div>
                {#if store.mcpFormDraft.env.length === 0}
                  <div class="mcp-empty-row">{i18n.t('settings.mcp.noEnv')}</div>
                {:else}
                  <div class="mcp-row-list">
                    {#each store.mcpFormDraft.env as row, index}
                      <div class="mcp-key-value-row">
                        <input class="form-input" type="text" value={row.key} placeholder={i18n.t('settings.mcp.keyPlaceholder')} oninput={(event) => store.updateMcpFormKeyValue('env', index, 'key', event.currentTarget.value)}>
                        <input class="form-input" type="text" value={row.value} placeholder={i18n.t('settings.mcp.valuePlaceholder')} oninput={(event) => store.updateMcpFormKeyValue('env', index, 'value', event.currentTarget.value)}>
                        <button type="button" class="mcp-remove-row" title={i18n.t('settings.mcp.removeRow')} onclick={() => store.removeMcpFormKeyValue('env', index)}>
                          <Icon name="close" size={14} />
                        </button>
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            </section>
          {:else}
            <section class="mcp-form-section">
              <div class="mcp-section-heading">
                <div>
                  <strong>{i18n.t('settings.mcp.httpSection')}</strong>
                  <span>{i18n.t('settings.mcp.httpSectionDesc')}</span>
                </div>
              </div>
              <div class="form-field">
                <label class="form-label" for="mcp-url">{i18n.t('settings.mcp.urlLabel')}</label>
                <input
                  class="form-input"
                  id="mcp-url"
                  type="text"
                  value={store.mcpFormDraft.url}
                  placeholder="https://example.com/mcp"
                  oninput={(event) => store.updateMcpFormField('url', event.currentTarget.value)}
                >
              </div>
              <div class="mcp-list-field">
                <div class="mcp-list-heading">
                  <span>{i18n.t('settings.mcp.headersLabel')}</span>
                  <button type="button" onclick={() => store.addMcpFormKeyValue('headers')}>
                    <Icon name="plus" size={13} />
                    {i18n.t('settings.mcp.addHeader')}
                  </button>
                </div>
                {#if store.mcpFormDraft.headers.length === 0}
                  <div class="mcp-empty-row">{i18n.t('settings.mcp.noHeaders')}</div>
                {:else}
                  <div class="mcp-row-list">
                    {#each store.mcpFormDraft.headers as row, index}
                      <div class="mcp-key-value-row">
                        <input class="form-input" type="text" value={row.key} placeholder={i18n.t('settings.mcp.headerKeyPlaceholder')} oninput={(event) => store.updateMcpFormKeyValue('headers', index, 'key', event.currentTarget.value)}>
                        <input class="form-input" type="text" value={row.value} placeholder={i18n.t('settings.mcp.headerValuePlaceholder')} oninput={(event) => store.updateMcpFormKeyValue('headers', index, 'value', event.currentTarget.value)}>
                        <button type="button" class="mcp-remove-row" title={i18n.t('settings.mcp.removeRow')} onclick={() => store.removeMcpFormKeyValue('headers', index)}>
                          <Icon name="close" size={14} />
                        </button>
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            </section>
          {/if}
        </div>
      {:else}
        <div class="mcp-json-editor form-field">
          <label class="form-label" for="mcp-json">{i18n.t('settings.mcp.jsonLabel')}</label>
          <textarea class="form-textarea" id="mcp-json" rows="16" spellcheck="false" placeholder={i18n.t('settings.mcp.jsonPlaceholder')} bind:value={store.mcpDialogJson} oninput={() => store.mcpDialogError = ''}></textarea>
          <div class="mcp-json-help">{i18n.t('settings.mcp.jsonHelp')}</div>
        </div>
      {/if}

      {#if store.mcpDialogError}
        <div class="form-error mcp-form-error" role="alert">{store.mcpDialogError}</div>
      {/if}
    </div>
    {#snippet footer()}
      <button class="btn btn--secondary" onclick={store.closeMCPDialog}>{i18n.t('settings.dialog.cancel')}</button>
      <button
        class="btn btn--primary"
        class:is-saving={store.saveStatus.mcp === 'saving'}
        class:is-saved={store.saveStatus.mcp === 'saved'}
        onclick={store.saveMCPServer}
        disabled={store.saveStatus.mcp === 'saving'}
      >
        {#if store.saveStatus.mcp === 'saving'}
          <Icon name="refresh" size={14} />
          {i18n.t('settings.mcp.saving')}
        {:else if store.saveStatus.mcp === 'saved'}
          <Icon name="check" size={14} />
          {i18n.t('settings.mcp.saved')}
        {:else}
          {i18n.t('settings.mcp.save')}
        {/if}
      </button>
    {/snippet}
  </Modal>
{/if}

<!-- 仓库管理对话框 -->
{#if store.showRepoDialogState}
  <Modal
    size="lg"
    closeOnEscape={true}
    onClose={store.closeRepoDialog}
  >
    {#snippet header()}
      <h3>{i18n.t('settings.repo.title')}</h3>
      <button class="modal-close" onclick={store.closeRepoDialog}>×</button>
    {/snippet}
    <div class="repo-add-form" style="margin-bottom: 24px;">
      <div class="form-field" style="flex: 1; margin-bottom: 0;">
        <label class="form-label" for="repo-url">{i18n.t('settings.repo.urlLabel')}</label>
        <input class="form-input" type="text" id="repo-url" placeholder={i18n.t('settings.repo.urlPlaceholder')} bind:value={store.repoAddUrl}>
      </div>
      <button class="btn btn--primary btn--sm" onclick={store.addRepository} disabled={store.repoAddLoading}>
        <Icon name="plus" size={14} />
        <span>{store.repoAddLoading ? i18n.t('settings.repo.adding') : i18n.t('settings.repo.add')}</span>
      </button>
    </div>
    <div class="repo-list-heading">
      <div class="repo-list-title">{i18n.t('settings.repo.addedRepos')}</div>
      <button class="btn btn--secondary btn--sm" onclick={store.checkSkillUpdates} disabled={store.skillUpdatesChecking || store.repositories.length === 0}>
        <Icon name="refresh" size={13} />
        <span>{i18n.t('settings.repo.refreshAll')}</span>
      </button>
    </div>
    <div class="repo-manage-list">
      {#if store.repositoriesLoading}
        <div class="loading-state">
          <Icon name="refresh" size={24} />
          <span>{i18n.t('settings.repo.loading')}</span>
        </div>
      {:else if store.repositories.length === 0}
        <div class="empty-state-sm">{i18n.t('settings.repo.noRepos')}</div>
      {:else}
        {#each store.repositories as repo (repo.id)}
          <div class="repo-item">
            <div class="repo-info">
              <div class="repo-name">{repo.name || repo.url}</div>
              <div class="repo-url">{repo.url}</div>
              <div class="repo-meta">
                <span>{i18n.t('settings.repo.skillCount', { count: repo.skillCount || 0 })}</span>
                {#if repo.commit}<span title={repo.commit}>{repo.commit.slice(0, 8)}</span>{/if}
                <span class:repo-ready={repo.syncStatus === 'ready'}>{repo.syncStatus === 'ready' ? i18n.t('settings.repo.ready') : i18n.t('settings.repo.notSynced')}</span>
              </div>
            </div>
            <div class="repo-actions">
              <button class="btn-icon btn-icon--sm" title={i18n.t('settings.repo.refresh')} onclick={() => store.refreshRepository(repo.id)} disabled={store.repositoryRefreshingIds.has(repo.id)}>
                <Icon name="refresh" size={14} />
              </button>
              <button class="btn-icon btn-icon--sm" title={i18n.t('settings.tools.delete')} onclick={() => store.deleteRepository(repo.id)}>
                <Icon name="close" size={14} />
              </button>
            </div>
          </div>
        {/each}
      {/if}
    </div>
    {#snippet footer()}
      <button class="btn btn--secondary btn--sm" onclick={store.closeRepoDialog}>{i18n.t('settings.repo.close')}</button>
    {/snippet}
  </Modal>
{/if}

<!-- Skill 库对话框 -->
{#if store.showSkillLibraryDialogState}
  <Modal
    size="lg"
    closeOnEscape={true}
    onClose={store.closeSkillLibraryDialog}
  >
    {#snippet header()}
      <h3>{i18n.t('settings.skillLibrary.title')}</h3>
      <button class="modal-close" onclick={store.closeSkillLibraryDialog}>×</button>
    {/snippet}
    <div class="skill-library-search">
      <div class="skill-library-search-row form-field" style="margin-bottom: 0;">
        <input class="form-input" type="text" placeholder={i18n.t('settings.skillLibrary.searchPlaceholder')} bind:value={store.skillSearchQuery}>
        <button class="btn btn--secondary btn--sm" onclick={store.checkSkillUpdates} disabled={store.skillUpdatesChecking}>
          <Icon name="refresh" size={14} />
          {i18n.t('settings.skillLibrary.checkUpdates')}
        </button>
        <button class="btn btn--secondary btn--sm" onclick={store.installLocalSkill} disabled={store.localSkillInstalling}>
          {#if store.localSkillInstalling}
            <Icon name="refresh" size={14} />
            {i18n.t('settings.skillLibrary.importing')}
          {:else}
            <Icon name="plus" size={14} />
            {i18n.t('settings.skillLibrary.localImport')}
          {/if}
        </button>
      </div>
    </div>
    <div class="skill-library-list">
      {#if store.localSkillInstallError}
        <div class="skill-library-warning skill-library-error">
          <div class="skill-library-warning-title">{store.localSkillInstallError}</div>
        </div>
      {/if}
      {#if store.skillLibraryFailedRepositoryCount > 0}
        <div class="skill-library-warning">
          <div class="skill-library-warning-title">
            {i18n.t('settings.skillLibrary.failedRepos', { count: store.skillLibraryFailedRepositoryCount })}
          </div>
        </div>
      {/if}
      {#if store.skillLibraryLoading}
        <div class="loading-state">
          <Icon name="refresh" size={32} />
          <span>{i18n.t('settings.skillLibrary.loadingSkills')}</span>
        </div>
      {:else if store.filteredLibrarySkills.length === 0}
        <div class="empty-state">
          <Icon name="tools" size={48} />
          <p>{i18n.t('settings.skillLibrary.noSkillsAvailable')}</p>
          <p class="empty-state-hint">{i18n.t('settings.skillLibrary.noSkillsHint')}</p>
        </div>
      {:else}
        {#each Object.entries(store.skillsByRepo) as [_, repoData]}
          <div class="skill-repo-group">
            <div class="skill-repo-title">{i18n.t('settings.skillLibrary.repoSkillCount', { name: repoData.name, count: repoData.skills.length })}</div>
            {#each repoData.skills as skill}
              <div class="skill-library-item">
                <div class="skill-library-icon">
                  <Icon name="tools" size={14} />
                </div>
                <div class="skill-library-info">
                  <div class="skill-library-name-line">
                    <div class="skill-library-name">{skill.name}</div>
                    {#if skill.installed}
                      <span class="skill-library-status" class:is-update={skill.updateAvailable}>
                        {skill.updateAvailable ? i18n.t('settings.skillLibrary.updateAvailable') : i18n.t('settings.skillLibrary.installed')}
                      </span>
                    {/if}
                  </div>
                  <div class="skill-library-desc" title={skill.description || ''}>{skill.description || ''}</div>
                  {#if skill.author || skill.version || skill.category}
                    <div class="skill-library-meta">
                      {#if skill.author}<span class="skill-library-meta-item">{i18n.t('settings.skillLibrary.author', { name: skill.author })}</span>{/if}
                      {#if skill.version}<span class="skill-library-meta-item">{i18n.t('settings.skillLibrary.version', { version: skill.version })}</span>{/if}
                      {#if skill.category}<span class="skill-library-meta-item">{i18n.t('settings.skillLibrary.category', { category: skill.category })}</span>{/if}
                    </div>
                  {/if}
                </div>
                <div class="skill-library-actions">
                  <button
                    class="btn btn--sm"
                    class:btn--primary={(!skill.installed || skill.updateAvailable) && !store.installingSkills.has(skill.fullName) && !store.skillUpdatingIds.has(skill.fullName)}
                    class:btn--secondary={!((!skill.installed || skill.updateAvailable) && !store.installingSkills.has(skill.fullName) && !store.skillUpdatingIds.has(skill.fullName))}
                    class:is-saving={store.installingSkills.has(skill.fullName) || store.skillUpdatingIds.has(skill.fullName)}
                    onclick={() => skill.installed ? store.updateSkill(skill.fullName) : store.installSkill(skill.fullName)}
                    disabled={(skill.installed && !skill.updateAvailable) || store.installingSkills.has(skill.fullName) || store.skillUpdatingIds.has(skill.fullName)}
                  >
                    {#if store.installingSkills.has(skill.fullName) || store.skillUpdatingIds.has(skill.fullName)}
                      <Icon name="refresh" size={14} />
                      {i18n.t('settings.skillLibrary.installing')}
                    {:else if skill.installed && skill.updateAvailable}
                      {skill.source === 'local' ? i18n.t('settings.tools.reloadSkill') : i18n.t('settings.tools.updateSkill')}
                    {:else if skill.installed}
                      {i18n.t('settings.skillLibrary.installed')}
                    {:else}
                      {i18n.t('settings.skillLibrary.install')}
                    {/if}
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/each}
      {/if}
    </div>
    {#snippet footer()}
      <button class="btn btn--secondary" onclick={store.closeSkillLibraryDialog}>{i18n.t('settings.skillLibrary.close')}</button>
    {/snippet}
  </Modal>
{/if}

{#if store.showLocalSkillFolderPicker}
  <Modal
    size="md"
    closeOnEscape={true}
    onClose={store.cancelLocalSkillFolderPicker}
    modalClass="folder-picker-modal"
    showHeader={false}
  >
    <WebFolderPicker
      title={i18n.t('settings.tools.selectLocalSkillFolder')}
      onSelect={store.handleLocalSkillFolderSelected}
      onCancel={store.cancelLocalSkillFolderPicker}
      disabled={store.localSkillInstalling}
    />
  </Modal>
{/if}

<!-- 重置 Token 确认对话框 -->
{#if store.showResetConfirm}
  <Modal
    size="sm"
    closeOnEscape={true}
    onClose={store.cancelResetStats}
    title={i18n.t('settings.resetConfirm.title')}
  >
    <p style="margin: 0; color: var(--foreground);">{i18n.t('settings.resetConfirm.workspaceMessage')}</p>
    <p style="margin: var(--space-2) 0 0; color: var(--foreground-muted); font-size: var(--text-sm);">{i18n.t('settings.resetConfirm.irreversible')}</p>
    {#snippet footer()}
      <button class="btn btn--secondary" onclick={store.cancelResetStats}>{i18n.t('settings.resetConfirm.cancel')}</button>
      <button class="btn btn--primary" onclick={store.confirmResetStats}>{i18n.t('settings.resetConfirm.confirm')}</button>
    {/snippet}
  </Modal>
{/if}

<!-- 通用确认对话框 -->
{#if store.showConfirmDialog}
  <Modal
    size="sm"
    closeOnEscape={true}
    onClose={store.handleConfirmNo}
    title={store.confirmDialogTitle}
  >
    <p style="margin: 0; color: var(--foreground);">{store.confirmDialogMessage}</p>
    {#snippet footer()}
      <button class="btn btn--secondary" onclick={store.handleConfirmNo}>{i18n.t('settings.confirmDialog.cancel')}</button>
      <button class="btn btn--primary" onclick={store.handleConfirmYes}>{i18n.t('settings.confirmDialog.confirm')}</button>
    {/snippet}
  </Modal>
{/if}

<style>
  /* ============================================
     Settings Panel - 优化后的样式
     ============================================ */


  .locale-selector {
    display: flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
    margin-left: auto;
  }

  .locale-btn {
    padding: var(--space-1) var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    cursor: pointer;
    transition: all var(--transition-fast);
    white-space: nowrap;
  }

  .locale-btn:hover {
    color: var(--foreground);
    background: var(--surface-3);
  }

  .locale-btn.active {
    background: var(--primary);
    color: var(--primary-foreground);
  }

  .locale-btn + .locale-btn {
    border-left: 1px solid var(--border);
  }


  /* 动画 */
  @keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }
  @keyframes slideUp { from { opacity: 0; transform: translateY(20px) scale(0.98); } to { opacity: 1; transform: translateY(0) scale(1); } }
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

  /* worker-dot 颜色已通过行内 style 动态注入 */

  /* 代理模型 Tabs - 放在标题右侧 */

  /* 空状态 */
  
  
  
  

  /* 加载状态 */
  .loading-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8);
    color: var(--foreground-muted);
    gap: var(--space-3);
  }
  .loading-state :global(svg) {
    animation: spin 1s linear infinite;
  }
  .loading-state span {
    font-size: var(--text-sm);
  }

  /* 表单字段 */
  .form-field { margin-bottom: var(--space-4); }
  .form-field:last-child { margin-bottom: 0; }
  .form-field > .form-label {
    display: block;
    margin-bottom: var(--space-2);
  }

  .form-error { font-size: 11px; color: var(--error); margin-top: 8px; line-height: 1.5; }

  .mcp-editor-title {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .mcp-editor-title h3 { margin: 0; }
  .mcp-editor-title span {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-normal);
    line-height: 1.45;
  }
  .mcp-editor {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-height: 0;
    overflow-y: auto;
    padding-right: 2px;
  }
  .mcp-mode-switch {
    display: flex;
    align-items: center;
    gap: var(--space-5);
    width: 100%;
    border-bottom: 1px solid var(--border);
  }
  .mcp-mode-switch button {
    position: relative;
    min-height: 36px;
    padding: 0 1px;
    border: 0;
    background: transparent;
    color: var(--foreground-muted);
    font: inherit;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    cursor: pointer;
    transition: color var(--transition-fast);
  }
  .mcp-mode-switch button::after {
    content: '';
    position: absolute;
    right: 0;
    bottom: -1px;
    left: 0;
    height: 2px;
    border-radius: 2px 2px 0 0;
    background: transparent;
  }
  .mcp-mode-switch button:hover { color: var(--foreground); }
  .mcp-mode-switch button.active { color: var(--foreground); }
  .mcp-mode-switch button.active::after { background: var(--primary); }
  .mcp-transport-switch {
    min-height: 34px;
  }
  .mcp-form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .mcp-form-grid {
    display: grid;
    grid-template-columns: minmax(0, 1.15fr) minmax(220px, 0.85fr);
    gap: var(--space-3);
  }
  .mcp-form-grid .form-field { margin-bottom: 0; }
  .mcp-field-label {
    display: block;
    margin-bottom: var(--space-2);
  }
  .mcp-form-options {
    display: flex;
    align-items: center;
    gap: var(--space-5);
    min-height: 36px;
    padding: var(--space-2) 0;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }
  .mcp-timeout-field,
  .mcp-enabled-field {
    min-height: 32px;
    box-sizing: border-box;
  }
  .mcp-timeout-field {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-shrink: 0;
  }
  .mcp-timeout-field label {
    display: inline-block;
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
  }
  .mcp-input-suffix {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .mcp-input-suffix .form-input {
    width: 64px;
    min-width: 0;
    height: 28px;
    padding: 4px 8px;
    font-size: var(--text-xs);
  }
  .mcp-input-suffix span { color: var(--foreground-muted); font-size: var(--text-xs); }
  .mcp-enabled-field {
    display: flex;
    align-items: center;
    justify-content: flex-start;
    gap: var(--space-3);
    min-width: 0;
    padding-left: var(--space-5);
    border-left: 1px solid var(--border);
  }
  .mcp-enabled-field > div {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .mcp-enabled-field strong { color: var(--foreground); font-size: var(--text-xs); }
  .mcp-enabled-field span {
    color: var(--foreground-muted);
    font-size: 11px;
    white-space: nowrap;
  }
  .mcp-form-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding-top: var(--space-1);
  }
  .mcp-section-heading > div {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .mcp-section-heading strong { color: var(--foreground); font-size: var(--text-sm); }
  .mcp-section-heading span { color: var(--foreground-muted); font-size: var(--text-xs); }
  .mcp-form-section .form-field { margin-bottom: 0; }
  .mcp-list-field {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .mcp-list-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .mcp-list-heading > span {
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }
  .mcp-list-heading button {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 26px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--primary);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .mcp-list-heading button:hover { color: var(--primary-hover); background: var(--surface-2); }
  .mcp-row-list { display: flex; flex-direction: column; gap: 6px; }
  .mcp-value-row,
  .mcp-key-value-row {
    display: grid;
    gap: 6px;
    align-items: center;
  }
  .mcp-value-row { grid-template-columns: minmax(0, 1fr) 30px; }
  .mcp-key-value-row { grid-template-columns: minmax(120px, 0.8fr) minmax(0, 1.2fr) 30px; }
  .mcp-value-row .form-input,
  .mcp-key-value-row .form-input {
    min-width: 0;
    height: 32px;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
  .mcp-remove-row {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }
  .mcp-remove-row:hover { color: var(--error); background: color-mix(in srgb, var(--error) 8%, transparent); }
  .mcp-empty-row {
    min-height: 32px;
    padding: 7px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--surface-2) 62%, transparent);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }
  .mcp-json-editor { margin-bottom: 0; }
  .mcp-json-editor .form-textarea {
    min-height: 320px;
    resize: vertical;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.55;
    tab-size: 2;
  }
  .mcp-json-help {
    margin-top: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }
  .mcp-form-error {
    margin-top: 0;
    padding: 8px 10px;
    border: 1px solid color-mix(in srgb, var(--error) 35%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--error) 7%, transparent);
  }

  @media (max-width: 640px) {
    .mcp-form-grid { grid-template-columns: 1fr; }
    .mcp-form-options {
      align-items: stretch;
      flex-direction: column;
      gap: var(--space-2);
    }
    .mcp-timeout-field,
    .mcp-enabled-field { width: 100%; }
    .mcp-enabled-field {
      justify-content: space-between;
      padding: var(--space-2) 0 0;
      border-top: 1px solid var(--border);
      border-left: 0;
    }
    .mcp-enabled-field span { white-space: normal; }
    .mcp-key-value-row { grid-template-columns: minmax(0, 1fr) 30px; }
    .mcp-key-value-row .form-input:nth-child(2) { grid-column: 1 / 2; }
    .mcp-key-value-row .mcp-remove-row { grid-column: 2; grid-row: 1 / 3; align-self: center; }
    .mcp-json-editor .form-textarea { min-height: 260px; }
  }

  /* 仓库管理 */
  .repo-add-form { display: flex; gap: 10px; align-items: flex-end; }
  .repo-add-form .form-field { flex: 1; margin-bottom: 0; }
  .repo-list-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 10px; }
  .repo-list-title { font-size: 13px; font-weight: 600; color: var(--foreground); }
  .repo-manage-list { display: flex; flex-direction: column; overflow: hidden; border: 1px solid var(--border); border-radius: 8px; }
  .repo-item {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: 12px;
    padding: 10px 12px;
    background: color-mix(in srgb, var(--surface-1) 94%, transparent);
    border-bottom: 1px solid var(--border);
    transition: background 0.15s ease;
  }
  .repo-item:last-child { border-bottom: 0; }
  .repo-item:hover {
    background: rgba(var(--foreground-rgb), 0.035);
  }
  .repo-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .repo-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .repo-url { font-size: var(--text-xs); color: var(--foreground-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .repo-meta { display: flex; gap: 10px; margin-top: 5px; font-size: var(--text-xs); color: var(--foreground-muted); }
  .repo-ready { color: var(--success); }
  .repo-actions { display: flex; gap: var(--space-2); flex-shrink: 0; }

  /* Skill 库 */
  .skill-library-search { margin-bottom: 24px; flex-shrink: 0; }
  .skill-library-search-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    gap: 12px;
    align-items: center;
  }
  .skill-library-warning {
    margin-bottom: var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--warning);
    background: color-mix(in srgb, var(--warning) 10%, var(--surface-1));
    border-radius: var(--radius-md);
  }
  .skill-library-error {
    border-color: var(--error);
    background: color-mix(in srgb, var(--error) 10%, var(--surface-1));
  }
  .skill-library-error .skill-library-warning-title {
    color: var(--error);
    margin-bottom: 0;
  }
  .skill-library-warning-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--warning);
    margin-bottom: 0;
  }

  .skill-repo-group { margin-bottom: var(--space-5); }
  .skill-repo-group:last-child { margin-bottom: 0; }
  .skill-repo-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: var(--space-3);
    padding-bottom: var(--space-2);
    border-bottom: 1px solid var(--border);
  }
  .skill-library-item {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: flex-start;
    gap: 12px;
    padding: 10px 12px;
    background: color-mix(in srgb, var(--surface-1) 94%, transparent);
    border: 1px solid var(--border);
    border-radius: 6px;
    margin-bottom: 6px;
    transition: background 0.15s ease, border-color 0.15s ease;
  }
  .skill-library-item:last-child { margin-bottom: 0; }
  .skill-library-item:hover {
    border-color: color-mix(in srgb, var(--primary) 35%, var(--border));
    background: rgba(var(--foreground-rgb), 0.03);
  }
  .skill-library-icon {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--primary-muted);
    border-radius: var(--radius-md);
    color: var(--primary);
    flex-shrink: 0;
  }
  .skill-library-info { min-width: 0; overflow: hidden; }
  .skill-library-name-line { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .skill-library-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .skill-library-status { padding: 1px 6px; border-radius: 4px; background: rgba(var(--success-rgb, 52, 199, 89), 0.1); color: var(--success); font-size: 10px; white-space: nowrap; }
  .skill-library-status.is-update { background: rgba(var(--warning-rgb, 255, 149, 0), 0.11); color: var(--warning); }
  .skill-library-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: 4px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; cursor: help; line-height: 1.5; }
  .skill-library-meta { display: flex; gap: var(--space-3); margin-top: var(--space-2); flex-wrap: wrap; }
  .skill-library-meta-item { font-size: var(--text-xs); color: var(--foreground-muted); }
  .skill-library-actions { flex-shrink: 0; align-self: center; }

  @media (max-width: 720px) {
    .skill-library-search-row {
      grid-template-columns: 1fr 1fr;
    }

    .skill-library-search-row .form-input {
      grid-column: 1 / -1;
    }

    .repo-add-form {
      align-items: stretch;
      flex-direction: column;
    }

  }

    /* 设置页：铺满工作台（侧栏 + 中栏）区域的整页视图，不是弹窗。
       与外壳共用同一套网格：导航列与侧栏同宽同材质，视觉上像侧栏切换成了设置导航。
       只用 opacity 做入场，不引入 transform，避免嵌套弹窗的 fixed 定位被改写参照系。 */
    .settings-page {
      /* 与工作台同级：占据外壳网格的整行（侧栏 + 中栏）。工作台被换下时它已不在网格流里，
         所以这里是正常的网格子项，不是叠加层，也不需要 z-index 或不透明底。 */
      grid-column: 1 / -1;
      grid-row: 1;
      display: grid;
      grid-template-columns: var(--sidebar-width, 320px) minmax(0, 1fr);
      gap: var(--shell-gap, 8px);
      min-width: 0;
      min-height: 0;
      animation: settingsPageIn var(--duration-fast, 150ms) var(--ease-out, ease-out);
    }

    .settings-page--away {
      display: none;
    }

    /* 桌面端：设置页也在整体内容框里，导航与页面之间不留缝隙，只有一条分隔线。
       窄屏（≤900px）设置页上下堆叠，沿用下面的移动端规则。 */
    @media (min-width: 901px) {
      :global(.web-workbench-shell--desktop) .settings-page {
        gap: 0;
      }

      :global(.web-workbench-shell--desktop) .settings-nav {
        border: 0;
        border-right: 1px solid var(--border);
        border-radius: 0;
      }

      :global(.web-workbench-shell--desktop) .settings-main {
        border: 0;
        border-radius: 0;
      }
    }

    .settings-nav {
      display: flex;
      flex-direction: column;
      gap: var(--space-1);
      min-height: 0;
      padding: var(--space-4);
      overflow-y: auto;
      border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      background: var(--magi-surface-sidebar);
    }

    /* 返回与侧栏里的“新建会话”同尺寸同位置，是这一列的第一行。 */
    .settings-back {
      display: flex;
      align-items: center;
      gap: var(--space-2);
      flex-shrink: 0;
      height: 30px;
      padding: 0 var(--space-2);
      border: none;
      border-radius: var(--radius-md);
      background: transparent;
      color: var(--foreground);
      font-size: var(--text-sm);
      font-weight: var(--font-medium);
      text-align: left;
      cursor: pointer;
      transition: background var(--transition-fast);
    }

    .settings-back :global(svg) {
      flex-shrink: 0;
      color: var(--foreground-muted);
    }

    .settings-back:hover:not(:disabled) {
      background: var(--surface-hover);
    }

    .settings-back:disabled {
      opacity: 0.5;
      cursor: progress;
    }

    .settings-nav-caption {
      flex-shrink: 0;
      padding: var(--space-4) var(--space-2) var(--space-1);
      color: var(--foreground-muted);
      font-size: var(--text-sm);
      font-weight: var(--font-semibold);
      letter-spacing: 0.04em;
    }

    .settings-nav-list {
      display: flex;
      flex-direction: column;
      gap: 2px;
    }

    .settings-nav-item {
      display: flex;
      align-items: center;
      gap: var(--space-3);
      flex-shrink: 0;
      height: 34px;
      padding: 0 var(--space-3);
      border: none;
      border-radius: var(--radius-md);
      background: transparent;
      color: var(--foreground-muted);
      font-size: var(--text-sm);
      text-align: left;
      white-space: nowrap;
      cursor: pointer;
      transition: background var(--transition-fast), color var(--transition-fast);
    }

    .settings-nav-item :global(svg) {
      flex-shrink: 0;
    }

    .settings-nav-item:hover {
      background: var(--surface-hover);
      color: var(--foreground);
    }

    .settings-nav-item.active {
      background: color-mix(in srgb, var(--surface-selected) 78%, transparent);
      color: var(--foreground);
      font-weight: var(--font-medium);
    }

    .settings-back:focus-visible,
    .settings-nav-item:focus-visible {
      outline: 2px solid var(--primary);
      outline-offset: 1px;
    }

    .settings-nav-footer {
      margin-top: auto;
      padding-top: var(--space-4);
      display: flex;
      flex-direction: column;
      gap: var(--space-3);
    }

    .settings-main {
      display: flex;
      flex-direction: column;
      min-width: 0;
      min-height: 0;
      overflow: hidden;
      border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      background: var(--magi-surface-main);
    }

    /* 大窗口下表单不无限拉宽：内容限制在 960px 居中，页头与内容对齐。 */
    .main-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: var(--space-4);
      flex-shrink: 0;
      padding: 18px max(28px, calc((100% - 960px) / 2));
      border-bottom: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
    }

    .header-breadcrumbs h2 {
      margin: 0;
      font-size: 18px;
      font-weight: 600;
      color: var(--foreground);
      white-space: nowrap;
    }

    .header-breadcrumbs {
      min-width: 0;
      flex: 1;
    }

    .header-title-group {
      min-width: 0;
      display: flex;
      align-items: baseline;
      gap: 12px;
    }

    .header-description {
      min-width: 0;
      color: var(--foreground-muted);
      font-size: 12px;
      font-weight: 500;
      line-height: 1.35;
    }

    .header-actions {
      display: flex;
      align-items: center;
      gap: 16px;
      flex: 0 0 auto;
    }

    .scroll-content {
      padding: 20px max(28px, calc((100% - 960px) / 2));
      overflow: hidden;
      flex: 1;
      display: flex;
      flex-direction: column;
      gap: 0; /* 间隔由各分类组件内部控制 */
      min-width: 0;
      min-height: 0;
    }

  @keyframes settingsPageIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  @media (prefers-reduced-motion: reduce) {
    .settings-page { animation: none; }
  }

  /* =========================================
     [UI/UX REFACTOR] GLOBAL TAB UNIFICATION
     ========================================= */

  /* 统一主行动按钮 (Buttons) */
  
  /* 窄屏：外壳去掉外边距，设置页变成整页——顶部是返回与横向滚动的分类，下面是内容。 */
  @media (max-width: 900px) {
    .settings-page {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: auto minmax(0, 1fr);
      gap: 0;
      background: var(--magi-surface-main);
    }

    .settings-nav {
      flex-direction: column;
      gap: var(--space-2);
      padding: calc(var(--space-3) + env(safe-area-inset-top, 0px)) var(--space-3) var(--space-3);
      overflow: visible;
      border: none;
      border-bottom: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
      border-radius: 0;
    }

    .settings-back {
      height: 40px;
    }

    .settings-nav-caption {
      display: none;
    }

    .settings-nav-list {
      flex-direction: row;
      gap: var(--space-2);
      overflow-x: auto;
      scrollbar-width: none;
      -webkit-overflow-scrolling: touch;
    }

    .settings-nav-list::-webkit-scrollbar {
      display: none;
    }

    .settings-nav-item {
      height: 36px;
      padding: 0 var(--space-4);
      border-radius: var(--radius-full);
    }

    .settings-nav-footer {
      display: none;
    }

    .settings-main {
      border: none;
      border-radius: 0;
    }

    .main-header {
      padding: 14px 16px;
    }

    .scroll-content {
      padding: 16px !important;
      padding-bottom: calc(24px + env(safe-area-inset-bottom, 0px)) !important;
      gap: 24px !important;
    }

    .header-actions .locale-selector .locale-btn {
      padding: 4px 8px;
      font-size: 12px;
    }
  }

  @media (max-width: 480px) {
    .main-header {
      align-items: flex-start;
      gap: 10px;
      padding: 14px 12px 14px 20px;
    }
    .header-title-group {
      align-items: flex-start;
      flex-direction: column;
      gap: 2px;
    }
    .header-description {
      display: -webkit-box;
      overflow: hidden;
      -webkit-box-orient: vertical;
      -webkit-line-clamp: 2;
      line-clamp: 2;
    }
    .header-actions {
      gap: 8px;
    }
  }
</style>

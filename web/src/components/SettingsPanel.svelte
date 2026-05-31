<script lang="ts">
import '../styles/settings.css';
import SettingsStatsTab from './SettingsStatsTab.svelte';
import SettingsRulesTab from './SettingsRulesTab.svelte';
import SettingsAgentsTab from './SettingsAgentsTab.svelte';
import SettingsModelTab from './SettingsModelTab.svelte';
import SettingsToolsTab from './SettingsToolsTab.svelte';
import Icon from './Icon.svelte';
import Modal from './Modal.svelte';
import { i18n } from '../stores/i18n.svelte';
import {
    updateAgentRuntimeSetting,
  } from '../web/agent-api';
import WebFolderPicker from '../web/WebFolderPicker.svelte';
import { getAgentColor } from '../lib/agent-colors';

  import { useSettingsStore } from '../stores/settings-store.svelte';
  
  interface Props {
    onClose?: () => void;
  }
  
  let { onClose }: Props = $props();

  const store = useSettingsStore({
    onClose: () => onClose?.(),
  });
</script>


<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="settings-overlay">
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="magi-settings-layout" onclick={(e) => e.stopPropagation()}>
    <!-- 左侧导航 -->
    <aside class="settings-sidebar">
      <div class="sidebar-header">
        <span class="settings-title">{i18n.t('settings.title')}</span>
      </div>
      <nav class="sidebar-nav">
        <button
          type="button"
          class="nav-item"
          class:active={store.activeTab === 'model'}
          aria-label={i18n.t('settings.zone.quickStart')}
          onclick={() => store.activeTab = 'model'}
        >
          <Icon name="model" size={16} />
          <span>{i18n.t('settings.zone.quickStart')}</span>
        </button>
        <button
          type="button"
          class="nav-item"
          class:active={store.activeTab === 'tools'}
          aria-label={i18n.t('settings.zone.capabilities')}
          onclick={() => store.activeTab = 'tools'}
        >
          <Icon name="tools" size={16} />
          <span>{i18n.t('settings.zone.capabilities')}</span>
        </button>
        <button
          type="button"
          class="nav-item"
          class:active={store.activeTab === 'agents'}
          aria-label={i18n.t('settings.zone.roles')}
          onclick={() => store.activeTab = 'agents'}
        >
          <Icon name="bot" size={16} />
          <span>{i18n.t('settings.zone.roles')}</span>
        </button>
        <button
          type="button"
          class="nav-item"
          class:active={store.activeTab === 'rules'}
          aria-label={i18n.t('settings.zone.preferences')}
          onclick={() => store.activeTab = 'rules'}
        >
          <Icon name="shield" size={16} />
          <span>{i18n.t('settings.zone.preferences')}</span>
        </button>
        <button
          type="button"
          class="nav-item"
          class:active={store.activeTab === 'stats'}
          aria-label={i18n.t('settings.zone.usage')}
          onclick={() => store.activeTab = 'stats'}
        >
          <Icon name="stats" size={16} />
          <span>{i18n.t('settings.zone.usage')}</span>
        </button>
      </nav>
      <div class="sidebar-footer">
        {#if store.userInfo && store.clientKind === 'vscode'}
          <div class="logout-section">
            <span class="user-info-text" title={store.userInfo}>{store.userInfo}</span>
            <button class="settings-btn secondary" onclick={store.logout}>{i18n.t('settings.logout')}</button>
          </div>
        {/if}
      </div>
    </aside>

    <!-- 右侧内容区 -->
    <main class="settings-main">
      <header class="main-header">
        <div class="header-breadcrumbs">
          {#if store.activeTab === 'model'}
            <div style="display: flex; align-items: baseline; gap: 12px;">
              <h2>{i18n.t('settings.zone.quickStart')}</h2>
              <span style="font-size: 12px; color: var(--foreground-muted); font-weight: 500;">{i18n.t('settings.zone.quickStartDesc')}</span>
            </div>
          {:else if store.activeTab === 'tools'}
            <div style="display: flex; align-items: baseline; gap: 12px;">
              <h2>{i18n.t('settings.zone.capabilities')}</h2>
              <span style="font-size: 12px; color: var(--foreground-muted); font-weight: 500;">{i18n.t('settings.zone.capabilitiesDesc')}</span>
            </div>
          {:else if store.activeTab === 'rules'}
            <div style="display: flex; align-items: baseline; gap: 12px;">
              <h2>{i18n.t('settings.zone.preferences')}</h2>
              <span style="font-size: 12px; color: var(--foreground-muted); font-weight: 500;">{i18n.t('settings.zone.preferencesDesc')}</span>
            </div>
          {:else if store.activeTab === 'stats'}
            <div style="display: flex; align-items: baseline; gap: 12px;">
              <h2>{i18n.t('settings.zone.usage')}</h2>
              <span style="font-size: 12px; color: var(--foreground-muted); font-weight: 500;">{i18n.t('settings.zone.usageDesc')}</span>
            </div>
          {:else if store.activeTab === 'agents'}
            <div style="display: flex; align-items: baseline; gap: 12px;">
              <h2>{i18n.t('settings.zone.roles')}</h2>
              <span style="font-size: 12px; color: var(--foreground-muted); font-weight: 500;">{i18n.t('settings.zone.rolesDesc')}</span>
            </div>
          {/if}
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
          <button class="btn-icon btn-icon--sm close-btn" onclick={store.closeSettings} title={i18n.t('settings.closeSettings')}>
            <Icon name="close" size={14} />
          </button>
        </div>
      </header>

      <!-- Tab 内容区域 -->
      <div class="settings-tab-content scroll-content" onscroll={() => { store.closeAllModelDropdowns(); }}>
      {#if store.activeTab === 'stats'}
        <!-- 统计 Tab -->
        <SettingsStatsTab totalInputTokens={store.totalInputTokens} totalOutputTokens={store.totalOutputTokens} totalTokens={store.totalTokens} isRefreshing={store.isRefreshing} refreshConnections={store.refreshConnections} showResetConfirmDialog={store.showResetConfirmDialog} modelStatuses={store.modelStatuses} statsDisplayKeys={store.statsDisplayKeys} getWorkerStats={store.getWorkerStats} getStatusClass={store.getStatusClass} getWorkerDisplayName={store.getWorkerDisplayName} statusTexts={store.statusTexts}
        />
      {:else if store.activeTab === 'model'}
        <!-- 模型配置 Tab -->
        <SettingsModelTab bind:modelConfigTab={store.modelConfigTab} orchConfig={store.orchConfig} compConfig={store.compConfig} workerConfigs={store.workerConfigs} workerModelTabs={store.workerModelTabs} modelStatuses={store.modelStatuses} saveStatus={store.saveStatus} testStatus={store.testStatus} fetchingModels={store.fetchingModels} bind:keyVisible={store.keyVisible} modelDropdownOpen={store.modelDropdownOpen} dropdownPosition={store.dropdownPosition} modelLists={store.modelLists} roleTemplates={store.roleTemplates} registryAgents={store.registryAgents} getBaseUrlPlaceholder={store.getBaseUrlPlaceholder} shouldRecommendStandardUrlMode={store.shouldRecommendStandardUrlMode} openModelDropdown={store.openModelDropdown} closeModelDropdown={store.closeModelDropdown} fetchModelList={store.fetchModelList} selectModel={store.selectModel} saveModelConfig={store.saveModelConfig} testModelConnection={store.testModelConnection} getStatusClass={store.getStatusClass} getStatusText={store.getStatusText} getWorkerDisplayName={store.getWorkerDisplayName}
          {getAgentColor} deleteEngine={store.deleteEngine} openAddEngineDialog={store.openAddEngineDialog} renameEngineDisplay={store.renameEngineDisplay}
        />
      {:else if store.activeTab === 'agents'}
        <!-- 角色管理 Tab -->
        <SettingsAgentsTab roleTemplates={store.roleTemplates} registryAgents={store.registryAgents} registryEngines={store.registryEngines}
          inheritModelLabel={store.orchConfig.model ?? ''} modelStatuses={store.modelStatuses}
          {getAgentColor} getWorkerDisplayName={store.getWorkerDisplayName} updateRoleEngine={store.updateRoleEngine}
        />
      {:else if store.activeTab === 'rules'}
        <!-- 规则 Tab -->
        <SettingsRulesTab bind:userRules={store.userRules} bind:newCustomRule={store.newCustomRule} SAFEGUARD_CATEGORIES={store.SAFEGUARD_CATEGORIES} getRulesForCategory={store.getRulesForCategory} toggleSafeguardRule={store.toggleSafeguardRule} updateSafeguardRuleAction={store.updateSafeguardRuleAction} removeCustomRule={store.removeCustomRule} addCustomRule={store.addCustomRule} userRulesSaveStatus={store.userRulesSaveStatus}
        />
      {:else if store.activeTab === 'tools'}
        <!-- 工具 Tab -->
        <SettingsToolsTab mcpServersHydrated={store.mcpServersHydrated} mcpServersLoading={store.mcpServersLoading} mcpServers={store.mcpServers} mcpExpandedServer={store.mcpExpandedServer} mcpServerTools={store.mcpServerTools} mcpRefreshingServers={store.mcpRefreshingServers} builtinTools={store.builtinTools} builtinToolsLoading={store.builtinToolsLoading} capabilityDependencies={store.capabilityDependencies} skills={store.skills} openMCPDialog={store.openMCPDialog} toggleMCPExpand={store.toggleMCPExpand} getMCPHealthLabel={store.getMCPHealthLabel} toggleMCPServer={store.toggleMCPServer} deleteMCPServer={store.deleteMCPServer} refreshMCPTools={store.refreshMCPTools} refreshBuiltinToolCatalog={store.refreshBuiltinToolCatalog} openSkillLibraryDialog={store.openSkillLibraryDialog} openRepoDialog={store.openRepoDialog} deleteSkill={store.deleteSkill}
        />
      {/if}
    </div>
  </main>
</div>
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
      <input type="text" bind:value={store.inputDialogValue} placeholder={i18n.t('settings.dialog.inputPlaceholder')}>
    </div>
    {#snippet footer()}
      <button class="apple-action-btn secondary" onclick={store.cancelInputDialog}>{i18n.t('settings.dialog.cancel')}</button>
      <button class="apple-action-btn" onclick={store.confirmInputDialog}>{i18n.t('settings.dialog.confirm')}</button>
    {/snippet}
  </Modal>
{/if}

<!-- MCP 对话框 -->
{#if store.showMCPDialogState}
  <Modal
    closeOnEscape={true}
    onClose={store.closeMCPDialog}
  >
    {#snippet header()}
      <h3>{store.mcpDialogIsEdit ? i18n.t('settings.mcp.editTitle') : i18n.t('settings.mcp.addTitle')}</h3>
      <button class="modal-close" onclick={store.closeMCPDialog}>×</button>
    {/snippet}
    <div class="form-field">
      <label for="mcp-json">{i18n.t('settings.mcp.jsonLabel')}</label>
      <textarea id="mcp-json" rows="12" placeholder={i18n.t('settings.mcp.jsonPlaceholder')} bind:value={store.mcpDialogJson} oninput={() => store.mcpDialogError = ''}></textarea>
      {#if store.mcpDialogError}
        <div class="form-error">{store.mcpDialogError}</div>
      {/if}
    </div>
    {#snippet footer()}
      <button class="apple-action-btn secondary" onclick={store.closeMCPDialog}>{i18n.t('settings.dialog.cancel')}</button>
      <button
        class="apple-action-btn"
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
        <label for="repo-url">{i18n.t('settings.repo.urlLabel')}</label>
        <input type="text" id="repo-url" placeholder={i18n.t('settings.repo.urlPlaceholder')} bind:value={store.repoAddUrl}>
      </div>
      <button class="apple-action-btn" onclick={store.addRepository} disabled={store.repoAddLoading}>
        <Icon name="plus" size={14} />
        <span>{store.repoAddLoading ? i18n.t('settings.repo.adding') : i18n.t('settings.repo.add')}</span>
      </button>
    </div>
    <div class="repo-list-title">{i18n.t('settings.repo.addedRepos')}</div>
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
              {#if repo.skillCount}
                <div class="repo-meta">{i18n.t('settings.repo.skillCount', { count: repo.skillCount })}</div>
              {/if}
            </div>
            <div class="repo-actions">
              <button class="btn-icon btn-icon--sm" title={i18n.t('settings.tools.delete')} onclick={() => store.deleteRepository(repo.id)}>
                <Icon name="close" size={14} />
              </button>
            </div>
          </div>
        {/each}
      {/if}
    </div>
    {#snippet footer()}
      <button class="apple-action-btn secondary" onclick={store.closeRepoDialog}>{i18n.t('settings.repo.close')}</button>
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
        <input type="text" placeholder={i18n.t('settings.skillLibrary.searchPlaceholder')} bind:value={store.skillSearchQuery}>
        <button class="apple-action-btn secondary" onclick={store.installLocalSkill} disabled={store.localSkillInstalling}>
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
      {#if store.skillLibraryFailedRepositories.length > 0}
        <div class="skill-library-warning">
          <div class="skill-library-warning-title">
            {i18n.t('settings.skillLibrary.failedRepos', { count: store.skillLibraryFailedRepositories.length })}
          </div>
          <div class="skill-library-warning-list">
            {#each store.skillLibraryFailedRepositories as repo}
              <div class="skill-library-warning-item">
                <strong>{repo.repositoryId}</strong>
              </div>
            {/each}
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
                  <div class="skill-library-name">{skill.name}</div>
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
                    class="settings-btn secondary"
                    class:primary={!skill.installed && !store.installingSkills.has(skill.fullName)}
                    class:is-saving={store.installingSkills.has(skill.fullName)}
                    onclick={() => store.installSkill(skill.fullName)}
                    disabled={skill.installed || store.installingSkills.has(skill.fullName)}
                  >
                    {#if store.installingSkills.has(skill.fullName)}
                      <Icon name="refresh" size={14} />
                      {i18n.t('settings.skillLibrary.installing')}
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
      <button class="settings-btn secondary" onclick={store.closeSkillLibraryDialog}>{i18n.t('settings.skillLibrary.close')}</button>
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
      onSelect={(path) => store.handleLocalSkillFolderSelected(path)}
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
    <p style="margin: 0; color: var(--foreground);">{i18n.t('settings.resetConfirm.message')}</p>
    <p style="margin: var(--space-2) 0 0; color: var(--foreground-muted); font-size: var(--text-sm);">{i18n.t('settings.resetConfirm.irreversible')}</p>
    {#snippet footer()}
      <button class="settings-btn secondary" onclick={store.cancelResetStats}>{i18n.t('settings.resetConfirm.cancel')}</button>
      <button class="settings-btn" onclick={store.confirmResetStats}>{i18n.t('settings.resetConfirm.confirm')}</button>
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
      <button class="settings-btn secondary" onclick={store.handleConfirmNo}>{i18n.t('settings.confirmDialog.cancel')}</button>
      <button class="settings-btn" onclick={store.handleConfirmYes}>{i18n.t('settings.confirmDialog.confirm')}</button>
    {/snippet}
  </Modal>
{/if}

<style>
  /* ============================================
     Settings Panel - 优化后的样式
     ============================================ */

  /* 基础面板布局 */
  .settings-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-modal);
    animation: fadeIn var(--duration-fast) var(--ease-out);
  }

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

  .settings-title {
    font-size: var(--text-xl);
    font-weight: var(--font-bold);
    color: var(--foreground);
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
  .form-field label {
    display: block;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .form-error { font-size: 11px; color: var(--error); margin-top: 8px; line-height: 1.5; }

  /* 仓库管理 */
  .repo-add-form { display: flex; gap: 10px; align-items: flex-end; }
  .repo-add-form .form-field { flex: 1; margin-bottom: 0; }
  .repo-list-title { font-size: 13px; font-weight: 600; color: var(--foreground); margin-bottom: 12px; }
  .repo-manage-list { display: flex; flex-direction: column; gap: 8px; }
  .repo-item {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    background: rgba(var(--vscode-editor-background-rgb, 255, 255, 255), 0.85);
    backdrop-filter: blur(16px);
    -webkit-backdrop-filter: blur(16px);
    border: 1px solid rgba(var(--foreground-rgb), 0.3); 
    box-shadow: 
      inset 0 0 0 1px rgba(255, 255, 255, 0.1),
      0 2px 8px rgba(0,0,0,0.1); 
    border-radius: 12px;
    transition: all 0.2s cubic-bezier(0.25, 0.8, 0.25, 1);
  }
  .repo-item:hover {
    border-color: rgba(var(--primary-rgb, 0, 122, 255), 0.6);
    background: rgba(var(--vscode-editor-background-rgb, 255, 255, 255), 1);
    box-shadow: 0 8px 20px rgba(0,0,0,0.12);
  }
  .repo-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .repo-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .repo-url { font-size: var(--text-xs); color: var(--foreground-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .repo-meta { font-size: var(--text-xs); color: var(--foreground-muted); }
  .repo-actions { display: flex; gap: var(--space-2); flex-shrink: 0; }

  /* Skill 库 */
  .skill-library-search { margin-bottom: 24px; flex-shrink: 0; }
  .skill-library-search-row {
    display: grid;
    grid-template-columns: 1fr auto;
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
    margin-bottom: var(--space-2);
  }
  .skill-library-warning-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .skill-library-warning-item {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    word-break: break-all;
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
    padding: 12px 16px;
    background: rgba(var(--vscode-editor-background-rgb, 255, 255, 255), 0.85);
    backdrop-filter: blur(16px);
    -webkit-backdrop-filter: blur(16px);
    border: 1px solid rgba(var(--foreground-rgb), 0.3); 
    box-shadow: 
      inset 0 0 0 1px rgba(255, 255, 255, 0.1),
      0 2px 8px rgba(0,0,0,0.1); 
    border-radius: 12px;
    margin-bottom: 8px;
    transition: all 0.2s cubic-bezier(0.25, 0.8, 0.25, 1);
  }
  .skill-library-item:last-child { margin-bottom: 0; }
  .skill-library-item:hover {
    border-color: rgba(var(--primary-rgb, 0, 122, 255), 0.6);
    background: rgba(var(--vscode-editor-background-rgb, 255, 255, 255), 1);
    box-shadow: 0 8px 20px rgba(0,0,0,0.12);
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
  .skill-library-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .skill-library-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: 4px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; cursor: help; line-height: 1.5; }
  .skill-library-meta { display: flex; gap: var(--space-3); margin-top: var(--space-2); flex-wrap: wrap; }
  .skill-library-meta-item { font-size: var(--text-xs); color: var(--foreground-muted); }
  .skill-library-actions { flex-shrink: 0; align-self: center; }

  @media (max-width: 720px) {
    .settings-overlay {
      align-items: stretch;
      justify-content: stretch;
      background: var(--overlay-heavy);
    }

  }

    /* Custom Layout CSS - UX/UI Fix */
    .magi-settings-layout {
      position: relative;
      background: var(--vscode-editor-background);
      border-radius: 12px;
      width: 90vw;
      max-width: 1050px;
      height: 800px;
      max-height: 800px;
      display: flex;
      flex-direction: row;
      box-shadow: 0 16px 40px rgba(0, 0, 0, 0.2);
      border: 1px solid var(--vscode-widget-border, rgba(0, 0, 0, 0.08));
      overflow: hidden;
      animation: magiSettingsIn 0.2s cubic-bezier(0.16, 1, 0.3, 1);
    }

    .settings-sidebar {
      width: 140px;
      background: var(--vscode-sideBar-background, rgba(0, 0, 0, 0.02));
      border-right: 1px solid var(--vscode-widget-border, rgba(0, 0, 0, 0.08));
      display: flex;
      flex-direction: column;
      padding: 20px 0;
      flex-shrink: 0;
    }

    .sidebar-header {
      padding: 0 24px 24px;
      border-bottom: 1px solid transparent;
    }
    .sidebar-header .settings-title {
      font-size: 18px;
      font-weight: 600;
      color: var(--vscode-foreground);
    }

    .sidebar-nav {
      display: flex;
      flex-direction: column;
      flex: 1;
      padding: 16px 12px;
      gap: 4px;
    }

    .sidebar-nav .nav-item {
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 10px 16px;
      border: none;
      background: transparent;
      border-radius: 8px;
      color: var(--vscode-foreground);
      font-size: 14px;
      font-weight: 500;
      cursor: pointer;
      opacity: 0.7;
      transition: all 0.2s ease;
      text-align: left;
    }

    @media (hover: hover) {
      .sidebar-nav .nav-item:hover {
        background: var(--vscode-list-hoverBackground, rgba(0, 0, 0, 0.04));
        opacity: 1;
      }
    }

    .sidebar-nav .nav-item.active {
      background: var(--vscode-list-activeSelectionBackground, rgba(0, 0, 0, 0.08));
      color: var(--vscode-list-activeSelectionForeground, var(--vscode-foreground));
      opacity: 1;
      font-weight: 600;
    }

    .sidebar-footer {
      padding: 16px 24px;
      margin-top: auto;
      display: flex;
      flex-direction: column;
      gap: 12px;
    }

    .settings-main {
      flex: 1;
      display: flex;
      flex-direction: column;
      min-width: 0;
      min-height: 0;
      background: var(--vscode-editor-background);
    }

    .main-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 20px 32px;
      border-bottom: 1px solid var(--vscode-widget-border, rgba(0, 0, 0, 0.08));
      background: var(--vscode-editor-background);
      z-index: 10;
    }

    .header-breadcrumbs h2 {
      margin: 0;
      font-size: 18px;
      font-weight: 600;
      color: var(--vscode-foreground);
    }

    .header-actions {
      display: flex;
      align-items: center;
      gap: 16px;
    }

    .scroll-content {
      padding: 20px; /* 压缩内边距 */
      overflow: hidden;
      flex: 1;
      display: flex;
      flex-direction: column;
      gap: 0; /* 移除巨大的间隔，由组件内部控制 */
      min-width: 0;
      min-height: 0;
    }

  @keyframes magiSettingsIn {
    from { opacity: 0; transform: scale(0.96) translateY(10px); }
    to { opacity: 1; transform: scale(1) translateY(0); }
  }

  /* =========================================
     [UI/UX REFACTOR] GLOBAL TAB UNIFICATION
     ========================================= */

  /* 统一主行动按钮 (Buttons) */
  
  @media (max-width: 768px) {
    .settings-sidebar {
      width: 64px;
      padding: 16px 0;
    }
    .sidebar-header {
      padding: 0 0 16px;
      display: flex;
      justify-content: center;
    }
    .sidebar-header .settings-title {
      display: none;
    }
    .sidebar-nav {
      padding: 16px 8px;
    }
    .sidebar-nav .nav-item {
      justify-content: center;
      padding: 12px;
    }
    .sidebar-nav .nav-item span {
      display: none;
    }
    .magi-settings-layout {
      width: 100vw;
      height: 100vh;
      height: 100dvh;
      max-width: none;
      max-height: none;
      border-radius: 0;
    }
    .main-header {
      padding: 16px 20px;
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
</style>

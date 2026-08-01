<script lang="ts">
  import { onMount } from 'svelte';
  import type { ComposerWorkspaceOption } from '../stores/composer-workspace.svelte';
  import { addToast, setCurrentTopTab } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    acceptCurrentGitContext,
    clearGitContextError,
    createGitContextBranch,
    gitContextBindingKey,
    gitContextState,
    refreshGitContext,
    switchGitContextBranch,
    type GitContextBinding,
  } from '../stores/git-context.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    workspace: ComposerWorkspaceOption | null;
    sessionId?: string;
    disabled?: boolean;
  }

  let { workspace, sessionId = '', disabled = false }: Props = $props();
  let open = $state(false);
  let search = $state('');
  let createMode = $state(false);
  let newBranch = $state('');

  const binding = $derived.by<GitContextBinding>(() => {
    if (!workspace) return {};
    return {
      workspaceId: workspace.workspaceId,
      workspacePath: workspace.rootPathRef?.trim() || workspace.rootPath.trim(),
      sessionId: sessionId.trim() || undefined,
    };
  });
  const bindingKey = $derived(gitContextBindingKey(binding));
  const stateMatches = $derived(Boolean(bindingKey) && gitContextState.bindingKey === bindingKey);
  const visible = $derived(stateMatches && gitContextState.loaded && gitContextState.isRepo);
  const busy = $derived(gitContextState.operation !== null);
  const interactionDisabled = $derived(disabled || busy);
  const filteredBranches = $derived.by(() => {
    const query = search.trim().toLowerCase();
    return gitContextState.branches.filter((branch) => !query || branch.toLowerCase().includes(query));
  });

  function statusItems(): string[] {
    const status = gitContextState.status;
    if (!status) return [];
    const items: string[] = [];
    if (status.staged > 0) items.push(i18n.t('input.branch.status.staged', { count: status.staged }));
    if (status.unstaged > 0) items.push(i18n.t('input.branch.status.unstaged', { count: status.unstaged }));
    if (status.untracked > 0) items.push(i18n.t('input.branch.status.untracked', { count: status.untracked }));
    if (status.conflicted > 0) items.push(i18n.t('input.branch.status.conflicted', { count: status.conflicted }));
    if (status.ahead > 0 || status.behind > 0) {
      items.push(i18n.t('input.branch.status.aheadBehind', { ahead: status.ahead, behind: status.behind }));
    }
    return items;
  }

  function controlTitle(): string {
    const branch = gitContextState.currentBranch || i18n.t('input.branch.detached');
    const items = statusItems();
    return `${branch} · ${items.length > 0 ? items.join(' · ') : i18n.t('input.branch.status.clean')}`;
  }

  async function toggle(): Promise<void> {
    open = !open;
    if (!open) return;
    clearGitContextError();
    try {
      await refreshGitContext(binding);
    } catch (error) {
      console.warn('[GitContextControl] 刷新 Git 上下文失败:', error);
    }
  }

  async function selectBranch(branch: string): Promise<void> {
    if (interactionDisabled || gitContextState.contextDrift) return;
    if (branch === gitContextState.currentBranch) {
      open = false;
      return;
    }
    try {
      const result = await switchGitContextBranch(binding, branch);
      if (!result.ok) {
        addToast('error', result.error?.message || i18n.t('input.branch.switchFailed'));
        return;
      }
      addToast('success', i18n.t('input.branch.switched', { branch }));
      open = false;
    } catch (error) {
      console.warn('[GitContextControl] 切换分支失败:', error);
      addToast('error', i18n.t('input.branch.switchFailed'));
    }
  }

  async function createBranch(): Promise<void> {
    const branch = newBranch.trim();
    if (!branch || interactionDisabled || gitContextState.contextDrift) return;
    try {
      const result = await createGitContextBranch(binding, branch);
      if (!result.ok) {
        addToast('error', result.error?.message || i18n.t('input.branch.createFailed'));
        return;
      }
      newBranch = '';
      createMode = false;
      open = false;
      addToast('success', i18n.t('input.branch.switched', { branch }));
    } catch (error) {
      console.warn('[GitContextControl] 创建分支失败:', error);
      addToast('error', i18n.t('input.branch.createFailed'));
    }
  }

  async function acceptContext(): Promise<void> {
    if (interactionDisabled) return;
    try {
      const result = await acceptCurrentGitContext(binding);
      if (!result.ok) {
        addToast('error', result.error?.message || i18n.t('input.branch.acceptContextFailed'));
        return;
      }
      addToast('success', i18n.t('input.branch.contextAccepted'));
    } catch (error) {
      console.warn('[GitContextControl] 接受 Git 基线失败:', error);
      addToast('error', i18n.t('input.branch.acceptContextFailed'));
    }
  }

  function openChanges(): void {
    open = false;
    setCurrentTopTab('edits');
  }

  $effect(() => {
    const key = bindingKey;
    if (!key) {
      void refreshGitContext({});
      return;
    }
    open = false;
    search = '';
    createMode = false;
    newBranch = '';
    void refreshGitContext(binding).catch((error) => {
      console.warn('[GitContextControl] 初始化 Git 上下文失败:', error);
    });
  });

  onMount(() => {
    const handleOutside = (event: PointerEvent) => {
      if (open && !(event.target instanceof Element && event.target.closest('.git-context-control'))) {
        open = false;
      }
    };
    const handleWorkspaceChanged = () => {
      void refreshGitContext(binding, { force: true }).catch((error) => {
        console.warn('[GitContextControl] 工作区变更后刷新 Git 上下文失败:', error);
      });
    };
    document.addEventListener('pointerdown', handleOutside, true);
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceChanged);
    return () => {
      document.removeEventListener('pointerdown', handleOutside, true);
      window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceChanged);
    };
  });
</script>

{#if visible}
  <div class="git-context-control">
    <button
      type="button"
      class="git-context-trigger"
      class:active={open}
      title={controlTitle()}
      aria-expanded={open}
      onclick={() => void toggle()}
    >
      <Icon name="git-branch" size={12} />
      <span class="git-context-branch">{gitContextState.currentBranch || i18n.t('input.branch.detached')}</span>
      {#if gitContextState.status?.conflicted}
        <span class="git-context-conflict">!{gitContextState.status.conflicted}</span>
      {:else if gitContextState.status?.hasUncommitted}
        <span class="git-context-dirty" aria-label={i18n.t('input.branch.status.dirty')}></span>
      {/if}
    </button>

    {#if open}
      <div class="git-context-popover" role="menu">
        <div class="git-context-search">
          <Icon name="search" size={12} />
          <input bind:value={search} placeholder={i18n.t('input.branch.searchPlaceholder')} aria-label={i18n.t('input.branch.searchPlaceholder')} />
        </div>

        {#if gitContextState.contextDrift}
          <div class="git-context-warning">
            <span>{i18n.t('input.branch.contextDrift')}</span>
            <button type="button" disabled={interactionDisabled} onclick={() => void acceptContext()}>{i18n.t('input.branch.acceptContext')}</button>
          </div>
        {/if}

        {#if gitContextState.error}
          <div class="git-context-error">
            <span>{gitContextState.error}</span>
            <button type="button" onclick={() => void refreshGitContext(binding, { force: true })}>{i18n.t('input.branch.retry')}</button>
          </div>
        {/if}

        <div class="git-context-list">
          {#each filteredBranches as branch (branch)}
            <button
              type="button"
              class="git-context-item"
              class:selected={gitContextState.currentBranch === branch}
              disabled={interactionDisabled || gitContextState.contextDrift}
              title={branch}
              onclick={() => void selectBranch(branch)}
            >
              <span>{branch}</span>
              {#if gitContextState.currentBranch === branch}<Icon name="check" size={12} />{/if}
            </button>
          {:else}
            <div class="git-context-empty">{i18n.t('input.branch.empty')}</div>
          {/each}
        </div>

        {#if createMode}
          <div class="git-context-create">
            <input
              bind:value={newBranch}
              placeholder={i18n.t('input.branch.createPlaceholder')}
              onkeydown={(event) => {
                if (event.key === 'Enter') {
                  event.preventDefault();
                  void createBranch();
                }
              }}
            />
            <button type="button" disabled={!newBranch.trim() || interactionDisabled || gitContextState.contextDrift} onclick={() => void createBranch()}>
              {i18n.t('input.branch.create')}
            </button>
          </div>
        {/if}

        <div class="git-context-actions">
          <button type="button" onclick={() => (createMode = !createMode)}><Icon name="plus" size={12} />{i18n.t('input.branch.newBranch')}</button>
          <button type="button" onclick={openChanges}><Icon name="file-edit" size={12} />{i18n.t('input.branch.viewChanges')}</button>
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .git-context-control { position: relative; min-width: 0; }
  .git-context-trigger {
    display: inline-flex; align-items: center; gap: 5px; max-width: 150px; height: 24px; padding: 0 8px;
    border: 1px solid var(--border-subtle); border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--surface-1) 88%, transparent); color: var(--foreground-muted); cursor: pointer;
    transition: background var(--transition-fast), border-color var(--transition-fast), color var(--transition-fast);
  }
  .git-context-trigger:hover, .git-context-trigger.active {
    border-color: color-mix(in srgb, var(--primary) 38%, var(--border-subtle));
    background: color-mix(in srgb, var(--primary) 12%, var(--surface-1));
    color: var(--primary);
  }
  .git-context-branch { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; }
  .git-context-dirty { width: 6px; height: 6px; flex: 0 0 6px; border-radius: 50%; background: var(--warning); }
  .git-context-conflict { color: var(--error); font-size: 10px; font-weight: var(--font-semibold); }
  .git-context-popover {
    position: absolute; bottom: calc(100% + 7px); left: 0; z-index: 34; width: min(280px, calc(100vw - 24px));
    padding: 6px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--dropdown-bg);
    box-shadow: 0 14px 36px rgba(0, 0, 0, 0.32);
  }
  .git-context-search { display: flex; align-items: center; gap: 7px; height: 32px; padding: 0 8px; border-bottom: 1px solid var(--border-subtle); color: var(--foreground-muted); }
  .git-context-search input, .git-context-create input { min-width: 0; flex: 1; border: 0; outline: 0; background: transparent; color: var(--foreground); font-size: 12px; }
  .git-context-list { max-height: 210px; overflow-y: auto; padding: 4px 0; }
  .git-context-item { display: flex; align-items: center; justify-content: space-between; gap: 8px; width: 100%; min-height: 30px; padding: 0 8px; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground); cursor: pointer; text-align: left; }
  .git-context-item span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .git-context-item:hover:not(:disabled), .git-context-item.selected { background: var(--surface-hover); }
  .git-context-item:disabled { cursor: not-allowed; opacity: 0.52; }
  .git-context-warning, .git-context-error { display: flex; flex-direction: column; gap: 5px; margin: 5px 0; padding: 7px 8px; border-radius: var(--radius-sm); color: var(--warning); background: color-mix(in srgb, var(--warning) 10%, transparent); font-size: 11px; line-height: 1.4; }
  .git-context-error { color: var(--error); background: color-mix(in srgb, var(--error) 10%, transparent); }
  .git-context-warning button, .git-context-error button { align-self: flex-start; padding: 0; border: 0; border-bottom: 1px solid currentColor; background: transparent; color: inherit; cursor: pointer; font-size: 11px; }
  .git-context-create { display: flex; gap: 6px; padding: 6px; border-top: 1px solid var(--border-subtle); }
  .git-context-create input { height: 28px; padding: 0 7px; border: 1px solid var(--border-subtle); border-radius: var(--radius-sm); }
  .git-context-create button, .git-context-actions button { display: inline-flex; align-items: center; gap: 5px; height: 28px; padding: 0 8px; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; font-size: 11px; }
  .git-context-create button:hover:not(:disabled), .git-context-actions button:hover { background: var(--surface-hover); color: var(--foreground); }
  .git-context-actions { display: flex; justify-content: space-between; gap: 4px; padding-top: 4px; border-top: 1px solid var(--border-subtle); }
  .git-context-empty { padding: 12px 8px; color: var(--foreground-muted); font-size: 11px; text-align: center; }
  @media (max-width: 640px) {
    .git-context-trigger { max-width: 96px; }
    .git-context-popover { position: fixed; bottom: calc(44px + env(safe-area-inset-bottom)); left: 12px; width: calc(100vw - 24px); }
  }
</style>

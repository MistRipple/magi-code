<script lang="ts">
  import { onMount } from 'svelte';
  import { addToast, getActiveInteractionType, messagesState } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    currentGitExpectedContext,
    gitContextBindingKey,
    gitContextState,
    refreshGitContext,
    runGitContextOperation,
    type GitContextBinding,
  } from '../stores/git-context.svelte';
  import {
    createWorkspaceWorktree,
    deleteWorkspaceBranch,
    fetchWorkspaceWorktrees,
    mergeWorkspaceBranch,
    previewWorkspaceMerge,
    removeWorkspaceWorktree,
    type GitMergePreview,
    type GitWorktree,
  } from '../web/agent-api';
  import Icon from './Icon.svelte';

  let advancedOpen = $state(false);
  let worktrees = $state<GitWorktree[]>([]);
  let worktreesLoading = $state(false);
  let worktreesLoaded = $state(false);
  let worktreeRequestSeq = 0;
  let worktreeMode = $state<'readOnly' | 'writable'>('readOnly');
  let worktreeBranch = $state('');
  let localError = $state<string | null>(null);

  const binding = $derived.by<GitContextBinding>(() => ({
    workspaceId: messagesState.currentWorkspaceId?.trim() || undefined,
    workspacePath: messagesState.currentWorkspacePath?.trim() || undefined,
    sessionId: messagesState.currentSessionId?.trim() || undefined,
  }));
  const bindingKey = $derived(gitContextBindingKey(binding));
  const stateMatches = $derived(Boolean(bindingKey) && gitContextState.bindingKey === bindingKey);
  const visible = $derived(stateMatches && gitContextState.loaded && gitContextState.isRepo);
  const interactionLocked = $derived(!messagesState.bootstrapped || Boolean(getActiveInteractionType()) || gitContextState.operation !== null);

  function notifyError(message: string): void {
    localError = message;
    addToast('error', message);
  }

  async function refreshRepository(): Promise<void> {
    localError = null;
    try {
      await refreshGitContext(binding, { force: true });
      if (advancedOpen) await loadWorktrees(true);
    } catch (error) {
      console.warn('[GitRepositoryPanel] 刷新仓库失败:', error);
      localError = i18n.t('input.branch.loadFailed');
    }
  }

  async function toggleAdvanced(): Promise<void> {
    advancedOpen = !advancedOpen;
    if (advancedOpen && !worktreesLoaded) await loadWorktrees();
  }

  async function loadWorktrees(force = false): Promise<void> {
    if (!visible || worktreesLoading || (worktreesLoaded && !force)) return;
    const requestBindingKey = bindingKey;
    const requestBinding = { ...binding };
    const requestSeq = ++worktreeRequestSeq;
    worktreesLoading = true;
    localError = null;
    try {
      const result = await fetchWorkspaceWorktrees(requestBinding);
      if (requestSeq !== worktreeRequestSeq || bindingKey !== requestBindingKey) {
        return;
      }
      if (!result.ok) {
        localError = result.error?.message || i18n.t('input.branch.worktreeLoadFailed');
        return;
      }
      worktrees = Array.isArray(result.data) ? result.data as GitWorktree[] : [];
      worktreesLoaded = true;
    } catch (error) {
      if (requestSeq !== worktreeRequestSeq || bindingKey !== requestBindingKey) {
        return;
      }
      console.warn('[GitRepositoryPanel] 读取 worktree 失败:', error);
      localError = i18n.t('input.branch.worktreeLoadFailed');
    } finally {
      if (requestSeq === worktreeRequestSeq && bindingKey === requestBindingKey) {
        worktreesLoading = false;
      }
    }
  }

  async function mergeBranch(target: string): Promise<void> {
    if (interactionLocked || gitContextState.contextDrift || gitContextState.status?.hasUncommitted) return;
    localError = null;
    try {
      const previewResult = await previewWorkspaceMerge(target, currentGitExpectedContext(), binding);
      if (!previewResult.ok) {
        notifyError(previewResult.error?.message || i18n.t('input.branch.mergePreviewFailed'));
        return;
      }
      const preview = previewResult.data as GitMergePreview;
      if (preview.alreadyUpToDate) {
        addToast('success', i18n.t('input.branch.alreadyUpToDate'));
        return;
      }
      const confirmed = window.confirm(i18n.t('input.branch.mergeConfirm', {
        branch: target,
        commits: preview.incomingCommitCount,
        files: preview.changedPaths.length,
        paths: preview.changedPaths.slice(0, 8).join('\n') || '—',
      }));
      if (!confirmed) return;
      const result = await runGitContextOperation('branchMerge', binding, () => (
        mergeWorkspaceBranch(target, false, currentGitExpectedContext(), binding)
      ));
      if (!result.ok) {
        const conflicts = result.error?.conflictedPaths?.join(', ');
        notifyError(conflicts
          ? i18n.t('input.branch.mergeConflict', { paths: conflicts })
          : result.error?.message || i18n.t('input.branch.mergeFailed'));
        return;
      }
      addToast('success', i18n.t('input.branch.merged', { branch: target }));
    } catch (error) {
      console.warn('[GitRepositoryPanel] 合并分支失败:', error);
      notifyError(i18n.t('input.branch.mergeFailed'));
    }
  }

  async function deleteLocalBranch(branch: string, force = false): Promise<void> {
    if (interactionLocked || gitContextState.contextDrift || branch === gitContextState.currentBranch) return;
    const confirmed = window.confirm(i18n.t(
      force ? 'input.branch.forceDeleteConfirm' : 'input.branch.deleteConfirm',
      { branch },
    ));
    if (!confirmed) return;
    localError = null;
    try {
      const result = await runGitContextOperation('branchDelete', binding, () => (
        deleteWorkspaceBranch(branch, { force, confirmForce: force }, currentGitExpectedContext(), binding)
      ));
      if (!result.ok) {
        if (!force && result.error?.kind === 'git_command_failed') {
          await deleteLocalBranch(branch, true);
          return;
        }
        notifyError(result.error?.message || i18n.t('input.branch.deleteFailed'));
        return;
      }
      addToast('success', i18n.t('input.branch.deleted', { branch }));
    } catch (error) {
      console.warn('[GitRepositoryPanel] 删除本地分支失败:', error);
      notifyError(i18n.t('input.branch.deleteFailed'));
    }
  }

  async function deleteRemoteBranch(fullName: string): Promise<void> {
    if (interactionLocked || gitContextState.contextDrift) return;
    const separator = fullName.indexOf('/');
    if (separator <= 0 || separator >= fullName.length - 1) return;
    if (!window.confirm(i18n.t('input.branch.remoteDeleteConfirm', { branch: fullName }))) return;
    const remote = fullName.slice(0, separator);
    const branch = fullName.slice(separator + 1);
    try {
      const result = await runGitContextOperation('remoteBranchDelete', binding, () => (
        deleteWorkspaceBranch(branch, { remote, confirmRemote: true }, currentGitExpectedContext(), binding)
      ));
      if (!result.ok) {
        notifyError(result.error?.message || i18n.t('input.branch.deleteFailed'));
        return;
      }
      addToast('success', i18n.t('input.branch.deleted', { branch: fullName }));
    } catch (error) {
      console.warn('[GitRepositoryPanel] 删除远程分支失败:', error);
      notifyError(i18n.t('input.branch.deleteFailed'));
    }
  }

  async function createWorktree(): Promise<void> {
    if (interactionLocked || gitContextState.contextDrift) return;
    try {
      const result = await runGitContextOperation('worktreeCreate', binding, () => (
        createWorkspaceWorktree(
          worktreeMode,
          {
            ...(worktreeMode === 'writable' && worktreeBranch.trim() ? { branch: worktreeBranch.trim() } : {}),
            allocationKey: messagesState.currentSessionId?.trim() || 'manual',
          },
          currentGitExpectedContext(),
          binding,
        )
      ));
      if (!result.ok) {
        notifyError(result.error?.message || i18n.t('input.branch.worktreeCreateFailed'));
        return;
      }
      worktreeBranch = '';
      addToast('success', i18n.t('input.branch.worktreeCreated'));
      await loadWorktrees(true);
    } catch (error) {
      console.warn('[GitRepositoryPanel] 创建 worktree 失败:', error);
      notifyError(i18n.t('input.branch.worktreeCreateFailed'));
    }
  }

  async function removeWorktree(path: string, force = false): Promise<void> {
    if (interactionLocked || path === gitContextState.worktreePath) return;
    const confirmed = window.confirm(i18n.t(
      force ? 'input.branch.worktreeForceRemoveConfirm' : 'input.branch.worktreeRemoveConfirm',
      { path },
    ));
    if (!confirmed) return;
    try {
      const result = await runGitContextOperation('worktreeRemove', binding, () => (
        removeWorkspaceWorktree(path, { force, confirmForce: force }, currentGitExpectedContext(), binding)
      ));
      if (!result.ok) {
        if (!force && result.error?.kind === 'git_command_failed') {
          await removeWorktree(path, true);
          return;
        }
        notifyError(result.error?.message || i18n.t('input.branch.worktreeRemoveFailed'));
        return;
      }
      addToast('success', i18n.t('input.branch.worktreeRemoved'));
      await loadWorktrees(true);
    } catch (error) {
      console.warn('[GitRepositoryPanel] 移除 worktree 失败:', error);
      notifyError(i18n.t('input.branch.worktreeRemoveFailed'));
    }
  }

  $effect(() => {
    const key = bindingKey;
    worktreeRequestSeq += 1;
    advancedOpen = false;
    worktrees = [];
    worktreesLoading = false;
    worktreesLoaded = false;
    localError = null;
    if (!key) return;
    void refreshGitContext(binding).catch((error) => {
      console.warn('[GitRepositoryPanel] 初始化仓库上下文失败:', error);
    });
  });

  onMount(() => {
    const handleWorkspaceChanged = () => void refreshRepository();
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceChanged);
    return () => window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceChanged);
  });
</script>

{#if visible}
  <section class="git-repository-panel" aria-label={i18n.t('input.branch.repositoryTitle')}>
    <div class="git-repository-summary">
      <div class="git-repository-identity">
        <Icon name="git-branch" size={14} />
        <div>
          <div class="git-repository-kicker">{i18n.t('input.branch.repositoryTitle')}</div>
          <div class="git-repository-branch" title={gitContextState.currentBranch || i18n.t('input.branch.detached')}>
            {gitContextState.currentBranch || i18n.t('input.branch.detached')}
          </div>
          <div class="git-repository-status">
            {gitContextState.status?.hasUncommitted ? i18n.t('input.branch.status.dirty') : i18n.t('input.branch.status.clean')}
            {#if gitContextState.status?.upstream} · {gitContextState.status.upstream}{/if}
          </div>
        </div>
      </div>
      <div class="git-repository-stats">
        {#if gitContextState.status?.additions || gitContextState.status?.deletions}
          <span class="add">+{gitContextState.status?.additions ?? 0}</span>
          <span class="del">-{gitContextState.status?.deletions ?? 0}</span>
        {/if}
        <button type="button" class:loading={gitContextState.loading} title={i18n.t('input.branch.refresh')} onclick={() => void refreshRepository()}>
          <Icon name="refresh" size={13} />
        </button>
      </div>
    </div>

    {#if localError || gitContextState.error}
      <div class="git-repository-error">{localError || gitContextState.error}</div>
    {/if}

    <button type="button" class="git-advanced-toggle" aria-expanded={advancedOpen} onclick={() => void toggleAdvanced()}>
      <span>{i18n.t('input.branch.advanced')}</span>
      <Icon name={advancedOpen ? 'chevron-up' : 'chevron-down'} size={12} />
    </button>

    {#if advancedOpen}
      <div class="git-advanced-content">
        <div class="git-section">
          <div class="git-section-title">{i18n.t('input.branch.localTitle')}</div>
          <div class="git-list">
            {#each gitContextState.structuredBranches.filter((branch) => !branch.isRemote) as branch (branch.fullRef)}
              <div class="git-row">
                <div class="git-row-main">
                  <span title={branch.name}>{branch.name}</span>
                  {#if branch.isCurrent}<small>{i18n.t('input.branch.current')}</small>{/if}
                  {#if branch.worktreePath && !branch.isCurrent}<small>{i18n.t('input.branch.inWorktree')}</small>{/if}
                </div>
                {#if !branch.isCurrent}
                  <div class="git-row-actions">
                    <button type="button" disabled={interactionLocked || Boolean(gitContextState.status?.hasUncommitted) || gitContextState.contextDrift} onclick={() => void mergeBranch(branch.name)}>{i18n.t('input.branch.merge')}</button>
                    <button type="button" class="danger" disabled={interactionLocked || Boolean(branch.worktreePath) || gitContextState.contextDrift} onclick={() => void deleteLocalBranch(branch.name)}>{i18n.t('input.branch.delete')}</button>
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        </div>

        {#if gitContextState.remoteBranches.length > 0}
          <div class="git-section">
            <div class="git-section-title">{i18n.t('input.branch.remoteTitle')}</div>
            <div class="git-list">
              {#each gitContextState.remoteBranches as branch (branch)}
                <div class="git-row">
                  <div class="git-row-main"><span title={branch}>{branch}</span></div>
                  <div class="git-row-actions">
                    <button type="button" class="danger" disabled={interactionLocked || gitContextState.contextDrift} onclick={() => void deleteRemoteBranch(branch)}>{i18n.t('input.branch.deleteRemote')}</button>
                  </div>
                </div>
              {/each}
            </div>
          </div>
        {/if}

        <div class="git-section">
          <div class="git-section-title">{i18n.t('input.branch.worktreeTitle')}</div>
          <div class="git-worktree-create">
            <select bind:value={worktreeMode} disabled={interactionLocked || gitContextState.contextDrift}>
              <option value="readOnly">{i18n.t('input.branch.worktreeReadOnly')}</option>
              <option value="writable">{i18n.t('input.branch.worktreeWritable')}</option>
            </select>
            {#if worktreeMode === 'writable'}
              <input bind:value={worktreeBranch} placeholder={i18n.t('input.branch.worktreeBranchPlaceholder')} disabled={interactionLocked || gitContextState.contextDrift} />
            {/if}
            <button type="button" disabled={interactionLocked || gitContextState.contextDrift} onclick={() => void createWorktree()}>{i18n.t('input.branch.worktreeCreate')}</button>
          </div>
          {#if worktreesLoading}
            <div class="git-empty">{i18n.t('input.branch.loading')}</div>
          {:else if worktrees.length === 0}
            <div class="git-empty">{i18n.t('input.branch.worktreeEmpty')}</div>
          {:else}
            <div class="git-list">
              {#each worktrees as worktree (worktree.path)}
                <div class="git-row">
                  <div class="git-row-main">
                    <span title={worktree.path}>{worktree.branch || i18n.t('input.branch.detached')}</span>
                    <small title={worktree.path}>{worktree.path}</small>
                  </div>
                  {#if worktree.path === gitContextState.worktreePath}
                    <small>{i18n.t('input.branch.currentWorktree')}</small>
                  {:else if worktree.managed}
                    <div class="git-row-actions"><button type="button" class="danger" disabled={interactionLocked} onclick={() => void removeWorktree(worktree.path)}>{i18n.t('input.branch.worktreeRemove')}</button></div>
                  {:else}
                    <small>{i18n.t('input.branch.externalWorktree')}</small>
                  {/if}
                </div>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    {/if}
  </section>
{/if}

<style>
  .git-repository-panel { flex: 0 0 auto; margin: var(--space-2); border: 1px solid var(--edits-row-border, var(--border-subtle)); border-radius: var(--radius-md); background: var(--edits-row-bg, var(--surface-1)); overflow: hidden; }
  .git-repository-summary { display: flex; align-items: center; justify-content: space-between; gap: 12px; min-height: 48px; padding: 7px 10px; }
  .git-repository-identity { display: flex; align-items: center; gap: 9px; min-width: 0; }
  .git-repository-branch { max-width: 320px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground); font-size: var(--text-sm); font-weight: var(--font-medium); }
  .git-repository-kicker { margin-bottom: 1px; color: var(--foreground-muted); font-size: 10px; font-weight: var(--font-semibold); }
  .git-repository-status { margin-top: 2px; color: var(--foreground-muted); font-size: var(--text-2xs); }
  .git-repository-stats { display: inline-flex; align-items: center; gap: 7px; font-size: var(--text-xs); font-weight: var(--font-semibold); }
  .git-repository-stats .add { color: var(--success); } .git-repository-stats .del { color: var(--error); }
  .git-repository-stats button { display: inline-flex; align-items: center; justify-content: center; width: 26px; height: 26px; padding: 0; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; }
  .git-repository-stats button:hover { background: var(--surface-hover); color: var(--foreground); }
  .git-repository-stats button.loading :global(svg) { animation: git-spin 900ms linear infinite; }
  .git-advanced-toggle { display: flex; align-items: center; justify-content: space-between; width: 100%; height: 30px; padding: 0 10px; border: 0; border-top: 1px solid var(--border-subtle); background: transparent; color: var(--foreground-muted); cursor: pointer; font-size: var(--text-xs); }
  .git-advanced-toggle:hover { background: var(--surface-hover); color: var(--foreground); }
  .git-advanced-content { display: flex; flex-direction: column; gap: 10px; max-height: 420px; padding: 10px; overflow-y: auto; border-top: 1px solid var(--border-subtle); }
  .git-section { min-width: 0; }
  .git-section-title { margin-bottom: 5px; color: var(--foreground-muted); font-size: var(--text-2xs); font-weight: var(--font-semibold); }
  .git-list { display: flex; flex-direction: column; border: 1px solid var(--border-subtle); border-radius: var(--radius-sm); overflow: hidden; }
  .git-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; min-height: 34px; padding: 5px 8px; background: var(--background); }
  .git-row + .git-row { border-top: 1px solid var(--border-subtle); }
  .git-row-main { display: flex; flex-direction: column; min-width: 0; }
  .git-row-main > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground); font-size: var(--text-xs); }
  .git-row small, .git-row-main small { max-width: 360px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground-muted); font-size: 10px; }
  .git-row-actions { display: inline-flex; gap: 4px; flex: 0 0 auto; }
  .git-row-actions button, .git-worktree-create button { height: 24px; padding: 0 7px; border: 1px solid var(--border-subtle); border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; font-size: 10px; }
  .git-row-actions button:hover:not(:disabled), .git-worktree-create button:hover:not(:disabled) { background: var(--surface-hover); color: var(--foreground); }
  .git-row-actions button.danger:hover:not(:disabled) { border-color: color-mix(in srgb, var(--error) 46%, var(--border-subtle)); color: var(--error); }
  button:disabled { cursor: not-allowed; opacity: 0.48; }
  .git-worktree-create { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 6px; }
  .git-worktree-create select, .git-worktree-create input { min-width: 130px; height: 26px; padding: 0 7px; border: 1px solid var(--border-subtle); border-radius: var(--radius-sm); background: var(--background); color: var(--foreground); font-size: 11px; }
  .git-repository-error { margin: 0 10px 8px; padding: 6px 8px; border-radius: var(--radius-sm); background: color-mix(in srgb, var(--error) 10%, transparent); color: var(--error); font-size: 11px; }
  .git-empty { padding: 9px; color: var(--foreground-muted); font-size: 11px; text-align: center; }
  @keyframes git-spin { to { transform: rotate(360deg); } }
  @media (max-width: 640px) {
    .git-repository-panel { margin: 6px; }
    .git-repository-branch { max-width: 180px; }
    .git-advanced-content { max-height: 52vh; }
    .git-row { align-items: flex-start; flex-direction: column; }
  }
</style>

<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { showFeedback } from '../lib/notifications';
  import { getAgentVersion } from '../web/agent-api';
  import { isDesktopRuntime } from '../lib/desktop-updater';
  import {
    checkForDesktopUpdate,
    desktopUpdaterState,
    downloadDesktopUpdate,
    restartWithDesktopUpdate,
    retryDesktopUpdate,
  } from '../stores/desktop-updater.svelte';

  const REPOSITORY_HOST_PATH = 'github.com/MistRipple/magi-code';
  const REPOSITORY_URL = `https://${REPOSITORY_HOST_PATH}`;

  // 仓库入口清单：名称、地址展示、跳转路径都由这里派生，不在模板里重复三份。
  const PROJECT_LINKS = [
    { id: 'repository', path: '', labelKey: 'settings.project.sourceCode' },
    { id: 'issues', path: '/issues', labelKey: 'settings.project.issues' },
    { id: 'releases', path: '/releases', labelKey: 'settings.project.releases' },
  ] as const;

  function openRepository(path: string): void {
    vscode.postMessage({ type: 'openLink', url: `${REPOSITORY_URL}${path}` });
  }

  // ---------- 版本 ----------
  const desktop = isDesktopRuntime();
  let core = $state<{ version: string; build: string } | null>(null);
  let detailsOpen = $state(false);
  let actionPending = $state(false);

  onMount(() => {
    void getAgentVersion()
      .then((info) => {
        core = { version: info.productVersion, build: info.buildIdentity };
      })
      .catch((error) => console.warn('[SettingsProjectTab] 读取后端版本失败:', error));
  });

  // 桌面端以应用版本为准；网页端只有后端版本。
  const displayVersion = $derived(desktop ? desktopUpdaterState.currentVersion : core?.version ?? '');
  const shortBuild = $derived(core?.build ? core.build.slice(0, 7) : '');

  type Tone = 'ok' | 'update' | 'busy' | 'error' | 'idle';
  interface UpdateStatus {
    tone: Tone;
    text: string;
    action: { label: string; run: () => Promise<unknown> } | null;
  }

  // 更新状态只从桌面端的更新状态机派生；这里不另存事实、不发明阶段。
  const updateStatus = $derived.by((): UpdateStatus | null => {
    if (!desktop) return null;
    const { phase, update, progress, error, lastCheckedAt } = desktopUpdaterState;
    if (update && !update.installability.installable) {
      return { tone: 'error', text: i18n.t('app.update.installationRequiredHint'), action: null };
    }
    switch (phase) {
      case 'checking':
        return { tone: 'busy', text: i18n.t('settings.update.checking'), action: null };
      case 'available':
        return {
          tone: 'update',
          text: i18n.t('settings.about.updateAvailable', { version: update?.version ?? '' }),
          action: {
            label: i18n.t('settings.update.available', { version: update?.version ?? '' }),
            run: downloadDesktopUpdate,
          },
        };
      case 'downloading':
        return {
          tone: 'busy',
          text: progress?.percent === undefined
            ? i18n.t('app.update.progressUnknown')
            : i18n.t('app.update.downloadingProgress', { percent: progress.percent }),
          action: null,
        };
      case 'ready':
        return {
          tone: 'update',
          text: i18n.t('app.update.readyHint'),
          action: { label: i18n.t('app.update.restartNow'), run: restartWithDesktopUpdate },
        };
      case 'installing':
        return { tone: 'busy', text: i18n.t('app.update.restarting'), action: null };
      case 'error':
        return {
          tone: 'error',
          text: error || i18n.t('app.update.retryHint'),
          action: { label: i18n.t('settings.update.retry'), run: retryDesktopUpdate },
        };
      default:
        return {
          tone: lastCheckedAt > 0 ? 'ok' : 'idle',
          text: lastCheckedAt > 0
            ? i18n.t('app.update.latestTitle')
            : i18n.t('settings.about.notChecked'),
          action: {
            label: i18n.t('settings.update.check'),
            run: () => checkForDesktopUpdate('manual'),
          },
        };
    }
  });

  const lastCheckedText = $derived(
    desktop && desktopUpdaterState.lastCheckedAt > 0
      ? new Date(desktopUpdaterState.lastCheckedAt).toLocaleString(i18n.locale, {
          month: 'numeric',
          day: 'numeric',
          hour: '2-digit',
          minute: '2-digit',
        })
      : '',
  );

  // 有新版本时给版本号一个醒目的提示点，不必打开详情也能看到。
  const chipTone = $derived<Tone>(updateStatus && updateStatus.tone !== 'ok' && updateStatus.tone !== 'idle' ? updateStatus.tone : 'ok');

  async function runUpdateAction(): Promise<void> {
    const action = updateStatus?.action;
    if (!action || actionPending) return;
    actionPending = true;
    try {
      await action.run();
    } finally {
      actionPending = false;
    }
  }

  async function copyVersionInfo(): Promise<void> {
    const parts = [`Magi v${displayVersion}`];
    if (core) parts.push(`core v${core.version}`, `build ${shortBuild}`);
    try {
      await navigator.clipboard.writeText(parts.join(' · '));
      showFeedback('success', i18n.t('settings.about.copied'), {
        presentation: 'toast',
        source: 'settings-about',
      });
    } catch (error) {
      console.warn('[SettingsProjectTab] 复制版本信息失败:', error);
      showFeedback('error', i18n.t('settings.about.copyFailed'), {
        presentation: 'toast',
        source: 'settings-about',
      });
    }
  }

  // 详情浮层：点击外部关闭；Esc 只关浮层（阻止冒泡，避免同时触发“返回”）。
  function handleWindowPointerDown(event: PointerEvent): void {
    if (!detailsOpen) return;
    const target = event.target instanceof Element ? event.target : null;
    if (!target?.closest('.about-version')) detailsOpen = false;
  }

  function handleVersionKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' && detailsOpen) {
      event.preventDefault();
      event.stopPropagation();
      detailsOpen = false;
    }
  }
</script>

<svelte:window onpointerdown={handleWindowPointerDown} />

<div class="settings-tab-inner project-tab">
  <!-- 身份区：产品名、简介与版本 -->
  <section class="about-hero" aria-label={i18n.t('settings.project.title')}>
    <div class="about-hero-text">
      <h3 class="about-name">Magi</h3>
      <p class="about-intro">{i18n.t('settings.project.intro')}</p>
    </div>

    {#if displayVersion}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="about-version" onkeydown={handleVersionKeydown}>
        <button
          class="about-version-chip"
          type="button"
          data-testid="about-version"
          aria-haspopup="dialog"
          aria-expanded={detailsOpen}
          aria-label={`${i18n.t('settings.about.versionDetails')} v${displayVersion}`}
          onclick={() => { detailsOpen = !detailsOpen; }}
        >
          <span class="about-dot about-dot--{chipTone}" aria-hidden="true"></span>
          <span class="about-version-number">v{displayVersion}</span>
          <Icon name="info" size={13} />
        </button>

        {#if detailsOpen}
          <div class="about-popover" role="dialog" aria-label={i18n.t('settings.about.versionDetails')} data-testid="about-version-details">
            <dl class="about-facts">
              {#if desktop}
                <div>
                  <dt>{i18n.t('settings.about.appVersion')}</dt>
                  <dd>v{desktopUpdaterState.currentVersion}</dd>
                </div>
              {/if}
              {#if core}
                <div>
                  <dt>{i18n.t('settings.about.coreVersion')}</dt>
                  <dd>v{core.version}</dd>
                </div>
                <div>
                  <dt>{i18n.t('settings.about.build')}</dt>
                  <dd class="about-mono">{shortBuild}</dd>
                </div>
              {/if}
            </dl>

            <div class="about-popover-section">
              {#if updateStatus}
                <div class="about-status">
                  <span class="about-dot about-dot--{updateStatus.tone}" aria-hidden="true"></span>
                  <span>{updateStatus.text}</span>
                </div>
                {#if lastCheckedText}
                  <div class="about-muted">{i18n.t('settings.about.lastChecked', { time: lastCheckedText })}</div>
                {/if}
                {#if updateStatus.action}
                  <button
                    class="btn btn--sm {updateStatus.tone === 'update' ? 'btn--primary' : 'btn--secondary'} about-action"
                    type="button"
                    disabled={actionPending}
                    onclick={() => void runUpdateAction()}
                  >
                    {updateStatus.action.label}
                  </button>
                {/if}
              {:else}
                <div class="about-muted">{i18n.t('settings.about.webManaged')}</div>
              {/if}
            </div>

            <button class="about-copy" type="button" onclick={() => void copyVersionInfo()}>
              {i18n.t('settings.about.copy')}
            </button>
          </div>
        {/if}
      </div>
    {/if}
  </section>

  <!-- 开源与反馈：三个入口做成并排的卡片 -->
  <section class="about-section" aria-labelledby="project-repository-title">
    <div class="about-section-heading">
      <h4 id="project-repository-title">{i18n.t('settings.project.repositoryLabel')}</h4>
      <p>{i18n.t('settings.project.repositoryDesc')}</p>
    </div>

    <ul class="about-links" aria-label={i18n.t('settings.project.moreLinks')}>
      {#each PROJECT_LINKS as link (link.id)}
        <li>
          <button
            class="about-link"
            type="button"
            data-testid={`project-open-${link.id}`}
            onclick={() => openRepository(link.path)}
          >
            <span class="about-link-name">{i18n.t(link.labelKey)}</span>
            <code class="about-link-url">{REPOSITORY_HOST_PATH}{link.path}</code>
            <span class="about-link-open">{i18n.t('settings.project.open')}</span>
          </button>
        </li>
      {/each}
    </ul>
  </section>
</div>

<style>
  .project-tab {
    gap: var(--space-6, 24px);
  }

  /* ---------- 身份区 ---------- */
  .about-hero {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-5);
    padding: var(--space-6, 24px);
    border: 1px solid var(--ind-border-card);
    border-radius: var(--ind-radius-card);
    background:
      radial-gradient(120% 140% at 0% 0%, color-mix(in srgb, var(--ind-tab-accent) 12%, transparent), transparent 62%),
      var(--ind-bg-card);
  }

  .about-hero-text {
    min-width: 0;
  }

  .about-name {
    margin: 0;
    color: var(--foreground);
    font-size: 30px;
    font-weight: 750;
    letter-spacing: -0.04em;
    line-height: 1.1;
  }

  .about-intro {
    max-width: 52ch;
    margin: var(--space-3) 0 0;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.7;
    text-wrap: pretty;
  }

  /* ---------- 版本 ---------- */
  .about-version {
    position: relative;
    flex-shrink: 0;
  }

  .about-version-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 30px;
    padding: 0 var(--space-3);
    border: 1px solid var(--ind-border-control, var(--border));
    border-radius: var(--radius-full);
    background: var(--ind-bg-control, transparent);
    color: var(--foreground);
    font-size: var(--text-sm);
    cursor: pointer;
    transition: border-color var(--transition-fast), background var(--transition-fast);
  }

  .about-version-chip:hover,
  .about-version-chip[aria-expanded='true'] {
    border-color: color-mix(in srgb, var(--ind-tab-accent) 55%, var(--border));
    background: color-mix(in srgb, var(--ind-tab-accent) 8%, var(--ind-bg-control, transparent));
  }

  .about-version-chip:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  .about-version-chip :global(svg) {
    color: var(--foreground-muted);
  }

  .about-version-number {
    font-variant-numeric: tabular-nums;
    font-weight: var(--font-medium);
  }

  .about-dot {
    flex-shrink: 0;
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    background: var(--foreground-muted);
  }

  .about-dot--ok { background: var(--success, #16a34a); }
  .about-dot--update { background: var(--warning, #d97706); }
  .about-dot--busy { background: var(--primary); }
  .about-dot--error { background: var(--error, #dc2626); }
  .about-dot--idle { background: var(--foreground-muted); }

  .about-popover {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    z-index: 5;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    width: 300px;
    max-width: calc(100vw - 32px);
    padding: var(--space-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--dropdown-bg);
    box-shadow: var(--shadow-lg);
  }

  .about-facts {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin: 0;
  }

  .about-facts > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .about-facts dt {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .about-facts dd {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    user-select: text;
  }

  .about-mono {
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  }

  .about-popover-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding-top: var(--space-3);
    border-top: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
  }

  .about-status {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--foreground);
    font-size: var(--text-sm);
    line-height: 1.45;
  }

  .about-muted {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .about-action {
    align-self: flex-start;
    margin-top: var(--space-1);
  }

  .about-copy {
    align-self: flex-start;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .about-copy:hover {
    color: var(--foreground);
  }

  .about-copy:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  /* ---------- 开源与反馈 ---------- */
  .about-section-heading h4 {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-base);
    font-weight: var(--font-bold);
  }

  .about-section-heading p {
    margin: var(--space-1) 0 0;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  /* 窄内容区一列纵向排列，足够宽时并排三列；不出现 2+1 的悬挂。 */
  .about-links {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-3);
    margin: var(--space-4) 0 0;
    padding: 0;
    list-style: none;
  }

  .about-link {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    grid-template-areas:
      'name open'
      'url open';
    align-items: center;
    column-gap: var(--space-3);
    row-gap: 2px;
    width: 100%;
    height: 100%;
    padding: var(--space-4);
    border: 1px solid var(--ind-border-card);
    border-radius: var(--ind-radius-card);
    background: var(--ind-bg-card);
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: border-color var(--transition-fast), background var(--transition-fast);
  }

  .about-link:hover {
    border-color: color-mix(in srgb, var(--ind-tab-accent) 50%, var(--border));
    background: color-mix(in srgb, var(--ind-tab-accent) 6%, var(--ind-bg-card));
  }

  .about-link:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  .about-link-name {
    grid-area: name;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .about-link-url {
    grid-area: url;
    max-width: 100%;
    overflow: hidden;
    color: var(--foreground-muted);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .about-link-open {
    grid-area: open;
    color: var(--ind-tab-accent);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
  }

  @media (min-width: 1200px) {
    .about-links {
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }
  }

  @media (max-width: 640px) {
    .about-hero {
      flex-direction: column;
      padding: var(--space-5);
    }

    .about-popover {
      right: auto;
      left: 0;
    }
  }
</style>

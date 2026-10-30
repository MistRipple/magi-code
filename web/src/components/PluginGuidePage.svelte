<script lang="ts">
  import { onDestroy } from 'svelte';
  /**
   * 插件开发规范内容页：加载随 daemon 发布的作者指南（Markdown），左侧章节目录、右侧限宽正文，
   * 支持复制原文与导出 .md，方便交给其他 agent 阅读。
   */
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { fetchPluginGuide } from '../web/plugin-api';
  import Icon from './Icon.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import SettingsDetailPage from './SettingsDetailPage.svelte';

  interface Props { onClose: () => void; }
  let { onClose }: Props = $props();

  let guide = $state<string | null>(null);
  let loadError = $state('');
  let guideCopied = $state(false);
  let copyResetTimer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(copyResetTimer));
  let guideActive = $state(0);
  let guideScroller = $state<HTMLDivElement | null>(null);

  onMount(() => {
    void fetchPluginGuide()
      .then((text) => { guide = text; })
      .catch((error) => { loadError = error instanceof Error ? error.message : String(error); });
  });

  /** 目录：取正文里的二级标题（跳过代码块里以 ## 开头的行）。 */
  const guideHeadings = $derived.by(() => {
    if (!guide) return [] as string[];
    const headings: string[] = [];
    let inFence = false;
    for (const line of guide.split('\n')) {
      if (/^```/.test(line)) { inFence = !inFence; continue; }
      const match = inFence ? null : /^##\s+(.+?)\s*$/.exec(line);
      if (match) headings.push(match[1].replace(/`/g, ''));
    }
    return headings;
  });

  function guideHeadingElements(): HTMLElement[] {
    return guideScroller ? [...guideScroller.querySelectorAll<HTMLElement>('h2')] : [];
  }

  function jumpToGuideHeading(index: number): void {
    guideHeadingElements()[index]?.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }

  function updateGuideActive(): void {
    if (!guideScroller) return;
    const top = guideScroller.getBoundingClientRect().top;
    let active = 0;
    guideHeadingElements().forEach((element, index) => {
      if (element.getBoundingClientRect().top - top <= 96) active = index;
    });
    guideActive = active;
  }

  /** 复制 Markdown 原文，方便直接交给其他 agent 读取。 */
  async function copyGuide(): Promise<void> {
    if (guide === null) return;
    try {
      await navigator.clipboard.writeText(guide);
    } catch {
      // 剪贴板 API 在失焦时可能被拒绝：退回到选区复制。
      const area = document.createElement('textarea');
      area.value = guide;
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.appendChild(area);
      area.select();
      document.execCommand('copy');
      area.remove();
    }
    guideCopied = true;
    clearTimeout(copyResetTimer);
    copyResetTimer = setTimeout(() => { guideCopied = false; }, 2500);
  }

  function exportGuide(): void {
    if (guide === null) return;
    const url = URL.createObjectURL(new Blob([guide], { type: 'text/markdown;charset=utf-8' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'magi-plugin-authoring.md';
    link.click();
    link.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 0);
  }
</script>

<!-- 加载、失败和正文共用同一个内容页，保留进入页面时的焦点恢复目标。 -->
<SettingsDetailPage title={i18n.t('settings.plugins.guide.toggle')} pageClass="plugin-guide-page" bodyClass={guide !== null && !loadError ? 'plugin-guide-page-body' : ''} {onClose}>
    {#snippet header()}
      <div class="guide-header">
        <h3>{i18n.t('settings.plugins.guide.toggle')}</h3>
        {#if guide !== null && !loadError}<div class="guide-actions">
          <button class="btn btn--secondary btn--sm" data-copy-feedback={guideCopied} data-testid="plugin-guide-copy" onclick={() => void copyGuide()}>
            <Icon name={guideCopied ? 'check' : 'copy'} size={13} />
            <span>{guideCopied ? i18n.t('settings.plugins.guide.copied') : i18n.t('settings.plugins.guide.copy')}</span>
          </button>
          <button class="btn btn--secondary btn--sm" data-testid="plugin-guide-export" onclick={exportGuide}>
            <Icon name="download" size={13} />
            <span>{i18n.t('settings.plugins.guide.export')}</span>
          </button>
        </div>{/if}
      </div>
    {/snippet}
    {#if loadError}
      <p class="settings-hint error" role="alert">{loadError}</p>
    {:else if guide !== null}
    <div class="plugin-guide" data-testid="plugin-guide">
      <nav class="guide-toc" aria-label={i18n.t('settings.plugins.guide.toc')}>
        {#each guideHeadings as heading, index (index)}
          <button type="button" class="guide-toc-item" class:active={guideActive === index} onclick={() => jumpToGuideHeading(index)}>{heading}</button>
        {/each}
      </nav>
      <div class="guide-scroll" bind:this={guideScroller} onscroll={updateGuideActive}>
        <article class="guide-article">
          <p class="guide-lead">{i18n.t('settings.plugins.guide.hint')}</p>
          <MarkdownContent content={guide} />
        </article>
      </div>
    </div>
    {:else}
    <p class="settings-hint" role="status">{i18n.t('common.loading')}</p>
    {/if}
</SettingsDetailPage>

<style>
  /* 右上角操作无底色，悬停才出现背景，与页面返回按钮一致 */
  .guide-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; width: 100%; }
  .guide-header h3 { margin: 0; }
  .guide-actions { display: flex; gap: 6px; align-items: center; }

  /* 开发规范阅读页：左侧章节目录、右侧正文；长表格与代码块只在自身内部滚动。 */

  :global(.plugin-guide-page) { overflow: hidden; min-height: 0; }
  :global(.plugin-guide-page .plugin-guide-page-body) { container-type: inline-size; container-name: plugin-guide; padding: 0; display: flex; min-height: 0; flex: 1 1 0; }
  .plugin-guide { display: grid; grid-template-columns: 208px minmax(0, 1fr); flex: 1; min-height: 0; min-width: 0; user-select: text; }
  .guide-toc { display: flex; flex-direction: column; gap: 2px; padding: var(--space-3) var(--space-2); overflow-y: auto; border-right: var(--magi-hairline) solid var(--border-subtle); }
  .guide-toc-item {
    overflow-wrap: anywhere; text-align: left; padding: 6px 10px; border: 0; border-radius: var(--radius-sm); background: transparent;
    color: var(--foreground-muted); font-size: var(--text-xs); line-height: 1.4; cursor: pointer;
  }
  .guide-toc-item:hover { background: var(--surface-hover); color: var(--foreground); }
  .guide-toc-item.active { background: var(--primary-muted); color: var(--foreground); font-weight: var(--font-medium); }
  .guide-scroll { min-width: 0; min-height: 0; overflow-y: auto; overflow-x: hidden; padding: 28px 44px 56px; }
  .guide-article { max-width: 740px; margin: 0 auto; min-width: 0; }
  .guide-lead { margin: 0 0 20px; color: var(--foreground-muted); font-size: var(--text-xs); line-height: 1.6; }
  .guide-article :global(.markdown-content) { font-size: 13px; line-height: 1.8; min-width: 0; }
  .guide-article :global(.markdown-content p) { margin: 0 0 14px; }
  .guide-article :global(.markdown-content h1) { font-size: 22px; line-height: 1.3; margin: 0 0 16px; }
  .guide-article :global(.markdown-content h2) { font-size: 16px; line-height: 1.4; margin: 40px 0 14px; padding-bottom: 8px; border-bottom: var(--magi-hairline) solid var(--border-subtle); scroll-margin-top: 8px; }
  .guide-article :global(.markdown-content h3) { font-size: 14px; margin: 28px 0 10px; }
  .guide-article :global(.markdown-content ul),
  .guide-article :global(.markdown-content ol) { margin: 0 0 14px; padding-left: 22px; }
  .guide-article :global(.markdown-content li) { margin: 8px 0; }
  .guide-article :global(.markdown-content table) { display: block; max-width: 100%; overflow-x: auto; margin: 16px 0; font-size: 12px; line-height: 1.6; }
  .guide-article :global(.markdown-content th),
  .guide-article :global(.markdown-content td) { padding: 8px 12px; vertical-align: top; }
  .guide-article :global(.markdown-content code) { overflow-wrap: anywhere; }
  .guide-article :global(.markdown-content td:first-child code) { white-space: nowrap; overflow-wrap: normal; }
  /* 规范里的代码块都是几行的短片段：自动换行并隐藏行号，不让长注释被截断。 */
  .guide-article :global(.markdown-content .code-pre),
  .guide-article :global(.markdown-content .code-text) { white-space: pre-wrap; overflow-wrap: anywhere; }
  .guide-article :global(.markdown-content .line-numbers) { display: none; }
  .guide-article :global(.markdown-content .code-block) { margin: 16px 0; max-width: 100%; }
  @container plugin-guide (max-width: 760px) {
    .plugin-guide { grid-template-columns: minmax(0, 1fr); }
    .guide-toc { display: none; }
    .guide-scroll { padding: 20px 20px 40px; }
  }
</style>

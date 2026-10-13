<!--
  Markdown 链接 renderer — 网页默认进入内置浏览器，文件继续走工作区预览
  接收 @humanspeak/svelte-markdown 传入的 href + title + children props
-->
<script lang="ts">
  import { getContext, setContext } from 'svelte';
  import type { Snippet } from 'svelte';
  import {
    dispatchFilePreviewEvent,
    FILE_PREVIEW_SCOPE_CONTEXT,
    type FilePreviewScopeReader,
    normalizeFileReferenceTarget,
  } from '../../lib/file-reference';
  import { vscode } from '../../lib/vscode-bridge';
  import { desktopContextMenu, type DesktopContextMenuDescriptor } from '../../lib/desktop-context-menu-contract';
  import { requestOpenUrlInBrowser } from '../../lib/browser-navigation';
  import { normalizeExternalWebUrl, openExternalUrl } from '../../lib/external-link';
  import { i18n } from '../../stores/i18n.svelte';
  import { addToast } from '../../stores/messages.svelte';
  import Icon from '../Icon.svelte';

  interface Props {
    href?: string;
    title?: string;
    children?: Snippet;
  }
  const { href = '', title = undefined, children }: Props = $props();
  setContext('markdown-link-context', true);
  const fileTarget = $derived(normalizeFileReferenceTarget(href));
  const webTarget = $derived(normalizeExternalWebUrl(href));
  const readFilePreviewScope = getContext<FilePreviewScopeReader | undefined>(FILE_PREVIEW_SCOPE_CONTEXT);

  function currentFilePreviewScope() {
    return readFilePreviewScope?.() ?? {};
  }

  function openTarget() {
    if (!href) {
      return;
    }
    if (fileTarget) {
      const scope = currentFilePreviewScope();
      if (dispatchFilePreviewEvent({ filepath: fileTarget, ...scope })) {
        return;
      }
      vscode.postMessage({ type: 'openFile', filepath: fileTarget, ...scope });
      return;
    }
    if (webTarget && requestOpenUrlInBrowser(webTarget)) {
      return;
    }
    // 非网页链接（`mailto:` 等）没有内置浏览器归宿，交给宿主的外部处理器；
    // IDE 宿主模式由 bridge 转发给宿主，Web/Desktop 由 bridge 落到
    // `openExternalUrl`（Desktop 进而交给系统邮件客户端）。
    vscode.postMessage({ type: 'openLink', url: href });
  }

  function handleClick(e: MouseEvent) {
    e.preventDefault();
    openTarget();
  }

  function handleOpenExternal(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (!webTarget) return;
    void openExternalUrl(webTarget).catch(() => {
      addToast('error', i18n.t('browser.error.openExternal'), undefined, { forceVisible: true });
    });
  }

  const contextDescriptor = $derived.by((): DesktopContextMenuDescriptor => fileTarget
    ? { kind: 'file', filePath: fileTarget, open: openTarget, fileScope: currentFilePreviewScope() }
    : { kind: 'link', url: href, open: openTarget });
</script>

<a
  {href}
  {title}
  class="md-link"
  use:desktopContextMenu={contextDescriptor}
  onclick={handleClick}
>{@render children?.()}</a>
{#if webTarget}
  <button
    type="button"
    class="md-link-external"
    title={i18n.t('browser.action.openExternal')}
    aria-label={i18n.t('browser.action.openExternal')}
    onclick={handleOpenExternal}
  ><Icon name="external-link" size={10} /></button>
{/if}

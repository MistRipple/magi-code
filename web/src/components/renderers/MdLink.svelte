<!--
  Markdown 链接 renderer — 在 webview 中通过 postMessage 安全打开
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

  interface Props {
    href?: string;
    title?: string;
    children?: Snippet;
  }
  const { href = '', title = undefined, children }: Props = $props();
  setContext('markdown-link-context', true);
  const fileTarget = $derived(normalizeFileReferenceTarget(href));
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
    vscode.postMessage({ type: 'openLink', url: href });
  }

  function handleClick(e: MouseEvent) {
    e.preventDefault();
    openTarget();
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

<!--
  Markdown 链接 renderer — 在 webview 中通过 postMessage 安全打开
  接收 @humanspeak/svelte-markdown 传入的 href + title + children props
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { vscode } from '../../lib/vscode-bridge';

  interface Props {
    href?: string;
    title?: string;
    children?: Snippet;
  }
  const { href = '', title = undefined, children }: Props = $props();

  function handleClick(e: MouseEvent) {
    e.preventDefault();
    if (href) {
      vscode.postMessage({ type: 'openLink', url: href });
    }
  }
</script>

<a
  {href}
  {title}
  class="md-link"
  onclick={handleClick}
>{@render children?.()}</a>


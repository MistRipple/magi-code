<script lang="ts">
  import { highlightCode } from '../../lib/code-highlighter';

  interface Props {
    diff?: string | null;
    ariaLabel?: string;
    fill?: boolean;
  }

  let { diff = '', ariaLabel = '', fill = false }: Props = $props();

  const diffCode = $derived(typeof diff === 'string' ? diff.trimEnd() : '');

  function escapeHtml(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  let highlightedDiffCode = $state('');

  $effect(() => {
    const source = diffCode;
    highlightedDiffCode = escapeHtml(source);
    if (!source) return;

    let cancelled = false;
    void highlightCode(source, 'diff').then((result) => {
      if (!cancelled && result !== null) highlightedDiffCode = result;
    }).catch((error) => {
      console.warn('[DiffCodeBlock] 高亮失败:', error);
    });
    return () => {
      cancelled = true;
    };
  });
</script>

<div class="diff-code-block" class:fill aria-label={ariaLabel || undefined}>
  <pre class="diff-code-scroll"><code class="language-diff">{@html highlightedDiffCode}</code></pre>
</div>

<style>
  .diff-code-block {
    min-width: 0;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md, 8px);
    background: var(--surface-1);
    color: var(--foreground);
    overflow: hidden;
  }

  .diff-code-block.fill {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  .diff-code-scroll {
    width: 100%;
    min-width: 0;
    max-height: min(60vh, 640px);
    margin: 0;
    padding: var(--space-3, 12px);
    overflow: auto;
    background: transparent;
    color: inherit;
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    line-height: 1.55;
    tab-size: 2;
  }

  .diff-code-block.fill .diff-code-scroll {
    flex: 1;
    min-height: 0;
    max-height: none;
  }

  .diff-code-scroll code {
    display: block;
    min-width: max-content;
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: inherit;
    font: inherit;
    white-space: pre;
  }

  .diff-code-scroll :global(.hljs-addition) {
    color: var(--success);
  }

  .diff-code-scroll :global(.hljs-deletion) {
    color: var(--error);
  }

  .diff-code-scroll :global(.hljs-meta) {
    color: var(--foreground-muted);
  }
</style>

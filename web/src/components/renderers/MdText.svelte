<script lang="ts">
  import { getContext } from 'svelte';
  import { splitFileReferenceText } from '../../lib/file-reference';
  import FileReferenceInline from './FileReferenceInline.svelte';

  // 作为 rawtext 渲染器：只处理叶子文本。带嵌套行内 token 的 text 节点
  // 由默认 text 渲染器递归渲染子节点，这里不能拦截，否则会丢掉加粗、行内代码和链接。
  interface Props {
    text?: string;
    raw?: string;
  }

  const { text = '', raw = '' }: Props = $props();
  const insideLink = getContext<boolean>('markdown-link-context') === true;
  const content = $derived(text || raw);
  const segments = $derived(splitFileReferenceText(content));
</script>

{#if insideLink}
  {content}
{:else}
  {#each segments as segment}
    {#if segment.kind === 'file'}
      <FileReferenceInline label={segment.text} target={segment.target} />
    {:else}
      {segment.text}
    {/if}
  {/each}
{/if}

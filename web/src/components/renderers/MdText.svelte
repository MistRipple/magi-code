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
  const streamingContext = getContext<{ readonly isStreaming: boolean } | undefined>('markdown-streaming');
  const content = $derived(text || raw);
  const segments = $derived(splitFileReferenceText(content));
  const isStreaming = $derived(streamingContext?.isStreaming === true);

  const segmenter = new Intl.Segmenter(undefined, { granularity: 'word' });
  const words = (value: string) => Array.from(segmenter.segment(value), (part) => part.segment);
  let burstStarted = 0;
  let burstIndex = 0;

  // Animate only newly mounted words. Never queue a delay based on total reply length.
  function inkArrival(node: HTMLSpanElement) {
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const now = performance.now();
    if (now - burstStarted > 30) {
      burstStarted = now;
      burstIndex = 0;
    }
    const animation = node.animate(
      [{ opacity: 0.35, filter: 'blur(6px)' }, { opacity: 1, filter: 'blur(0)' }],
      { duration: 90, delay: Math.min(burstIndex++, 7) * 30, easing: 'cubic-bezier(.215,.61,.355,1)', fill: 'backwards' },
    );
    return { destroy: () => animation.cancel() };
  }
</script>


{#if insideLink}
  {content}
{:else}
  {#each segments as segment}
    {#if segment.kind === 'file'}
      <FileReferenceInline label={segment.text} target={segment.target} />
    {:else if isStreaming}
      {#each words(segment.text) as word, index (index)}
        <span use:inkArrival>{word}</span>
      {/each}
    {:else}
      {segment.text}
    {/if}
  {/each}
{/if}

<!--
  Markdown 图片 renderer — 安全图片展示，限制最大宽度
  接收 @humanspeak/svelte-markdown 传入的 href + title + text props
-->
<script lang="ts">
  interface Props {
    href?: string;
    title?: string;
    text?: string;
  }
  const { href = '', title = undefined, text = '' }: Props = $props();
</script>

<img
  src={href}
  alt={text}
  {title}
  loading="lazy"
  class="md-image"
/>

<style>
  .md-image {
    max-width: 100%;
    height: auto;
    border-radius: var(--radius-sm);
  }
</style>


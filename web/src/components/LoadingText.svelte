<script lang="ts">
  import Icon from './Icon.svelte';
  /** 等待响应的统一表现：旋转圆圈 + 说明文字（消息响应的三点动画除外）。 */
  interface Props { text: string; size?: number }
  let { text, size = 14 }: Props = $props();
</script>

<span class="loading-text" role="status"><Icon name="loader" {size} class="spinning" /><span>{text}</span></span>

<style>
  .loading-text { display: inline-flex; align-items: center; gap: var(--space-2); min-width: 0; }
</style>

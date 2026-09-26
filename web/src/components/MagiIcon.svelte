<script lang="ts">
  interface Props {
    size?: number | string;
    title?: string;
    class?: string;
  }

  let {
    size = 32,
    title = 'Magi',
    class: className = '',
  }: Props = $props();
</script>

<img
  class={`magi-icon ${className}`}
  src="/brand/magi-icon.png"
  width={size}
  height={size}
  alt={title}
  draggable="false"
  decoding="async"
  fetchpriority="high"
/>

<style>
  .magi-icon {
    display: block;
    flex: 0 0 auto;
    object-fit: contain;
    user-select: none;
  }
</style>

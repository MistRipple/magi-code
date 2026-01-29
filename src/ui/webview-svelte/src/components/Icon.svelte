<script lang="ts">
  import { icons, type IconName } from '../lib/icons';

  interface Props {
    name: IconName;
    size?: number;
    class?: string;
  }

  let { name, size = 16, class: className = '' }: Props = $props();

  const pathData = $derived(icons[name] || '');
</script>

<svg 
  viewBox="0 0 16 16" 
  width={size} 
  height={size} 
  fill="currentColor"
  class={className}
>
  {@html pathData}
</svg>

<style>
  svg {
    display: inline-block;
    vertical-align: middle;
    flex-shrink: 0;
  }
</style>


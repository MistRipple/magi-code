<script lang="ts">
  import { icons, iconStyle, type IconName } from '../lib/icons';

  interface Props {
    name: IconName;
    size?: number;
    class?: string;
  }

  let { name, size = 16, class: className = '' }: Props = $props();

  const pathData = $derived(icons[name] || '');
</script>

<svg 
  viewBox={iconStyle.viewBox}
  width={size} 
  height={size} 
  fill={iconStyle.fill}
  stroke={iconStyle.stroke}
  stroke-width={iconStyle.strokeWidth}
  stroke-linecap={iconStyle.strokeLinecap}
  stroke-linejoin={iconStyle.strokeLinejoin}
  style={`fill:${iconStyle.fill};stroke:${iconStyle.stroke};stroke-width:${iconStyle.strokeWidth};stroke-linecap:${iconStyle.strokeLinecap};stroke-linejoin:${iconStyle.strokeLinejoin}`}
  class={`magi-icon ${className}`}
  data-icon={name}
  aria-hidden="true"
  focusable="false"
>
  {@html pathData}
</svg>

<style>
  svg {
    display: inline-block;
    vertical-align: -0.125em;
    flex-shrink: 0;
  }
</style>


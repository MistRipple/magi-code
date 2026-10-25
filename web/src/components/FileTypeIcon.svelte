<script lang="ts">
  import Icon from './Icon.svelte';
  import { getFileTypeVisual } from '../lib/file-type';
  import { fileTypeStyles } from '../lib/file-type-style';

  let { path, size = 14 }: { path: string; size?: number } = $props();
  const visual = $derived(getFileTypeVisual(path));
  const appearance = $derived(fileTypeStyles[visual.kind]);
</script>

<span
  class={`file-type-icon file-type-icon--${visual.kind}`}
  class:file-type-icon--badge={!!appearance.badge}
  style={`--file-type-icon-size:${size}px;--file-type-light:${appearance.light};--file-type-dark:${appearance.dark}`}
  title={visual.label}
  aria-hidden="true"
>
  {#if appearance.brandPath}
    <svg class="file-type-brand" viewBox="0 0 24 24" width={size} height={size} style={`width:${size}px;height:${size}px;fill:currentColor;stroke:none`} focusable="false" aria-hidden="true"><path d={appearance.brandPath}/></svg>
  {:else if appearance.badge}
    {appearance.badge}
  {:else}
    <Icon name={visual.icon ?? 'file'} {size} />
  {/if}
</span>

<style>
  .file-type-icon {
    --file-type-color: var(--file-type-light);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--file-type-icon-size, 14px);
    height: var(--file-type-icon-size, 14px);
    flex: 0 0 auto;
    color: var(--file-type-color);
    vertical-align: -0.125em;
    line-height: 1;
    user-select: none;
  }
  :global(.theme-dark) .file-type-icon { --file-type-color: var(--file-type-dark); }
  .file-type-icon--badge {
    border-radius: 3px;
    background: color-mix(in srgb, var(--file-type-color) 12%, transparent);
    font-family: var(--font-mono, monospace);
    font-size: max(7px, calc(var(--file-type-icon-size, 14px) * .5));
    font-weight: 600;
    letter-spacing: -.06em;
  }
  .file-type-brand { display:block; flex:none; }
</style>

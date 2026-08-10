<script lang="ts">
  import Icon from './Icon.svelte';
  import { getFileTypeVisual } from '../lib/file-type';

  interface Props {
    path: string;
    size?: number;
  }

  let { path, size = 14 }: Props = $props();
  const visual = $derived(getFileTypeVisual(path));
</script>

{#if visual.glyph}
  <span
    class={`file-type-icon file-type-icon--${visual.kind}`}
    style={`--file-type-icon-size: ${size}px`}
    title={visual.label}
    aria-hidden="true"
  >{visual.glyph}</span>
{:else}
  <span
    class={`file-type-icon file-type-icon--${visual.kind}`}
    style={`--file-type-icon-size: ${size}px`}
    title={visual.label}
    aria-hidden="true"
  >
    <Icon name={visual.icon ?? 'file'} size={Math.max(10, size - 4)} />
  </span>
{/if}

<style>
  .file-type-icon {
    box-sizing: border-box;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--file-type-icon-size, 14px);
    height: var(--file-type-icon-size, 14px);
    flex-shrink: 0;
    border: 1px solid color-mix(in srgb, var(--file-type-color, var(--foreground-muted)) 62%, transparent);
    border-radius: 3px;
    background: color-mix(in srgb, var(--file-type-color, var(--foreground-muted)) 14%, transparent);
    color: var(--file-type-color, var(--foreground-muted));
    font-family: var(--font-mono);
    font-size: max(7px, calc(var(--file-type-icon-size, 14px) * 0.55));
    font-weight: 700;
    letter-spacing: -0.08em;
    line-height: 1;
    text-align: center;
    overflow: hidden;
  }

  .file-type-icon--svelte { --file-type-color: #ff6b84; }
  .file-type-icon--typescript { --file-type-color: #4e9bd6; }
  .file-type-icon--javascript { --file-type-color: #e6b93f; }
  .file-type-icon--rust { --file-type-color: #e27b44; }
  .file-type-icon--python { --file-type-color: #5795c1; }
  .file-type-icon--go { --file-type-color: #55b4c7; }
  .file-type-icon--java,
  .file-type-icon--html { --file-type-color: #e9704c; }
  .file-type-icon--markdown { --file-type-color: #55b96f; }
  .file-type-icon--json,
  .file-type-icon--toml { --file-type-color: #dc9448; }
  .file-type-icon--css { --file-type-color: #6197d5; }
  .file-type-icon--yaml,
  .file-type-icon--shell,
  .file-type-icon--sql { --file-type-color: #ad8bc1; }
  .file-type-icon--docker { --file-type-color: #4aa6d4; }
  .file-type-icon--git { --file-type-color: #d97e4d; }
  .file-type-icon--env { --file-type-color: #74b566; }
  .file-type-icon--text { --file-type-color: #7aa6b7; }
  .file-type-icon--image { --file-type-color: #bb81d1; }
  .file-type-icon--binary { --file-type-color: #a887d5; }
  .file-type-icon--generic { --file-type-color: #8893a4; }

  .file-type-icon :global(svg) {
    color: inherit;
    width: calc(var(--file-type-icon-size, 14px) - 4px);
    height: calc(var(--file-type-icon-size, 14px) - 4px);
  }
</style>

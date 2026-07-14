<script lang="ts">
  interface Props {
    width?: number;
    height?: number;
    title?: string;
    class?: string;
  }

  let {
    width = 108,
    height = 30,
    title = 'Magi',
    class: className = '',
  }: Props = $props();
</script>

<svg
  class={`magi-wordmark ${className}`}
  viewBox="0 0 334 96"
  width={width}
  height={height}
  fill="currentColor"
  role="img"
  aria-label={title}
>
  <path d="M4 12h20l34 27 36-27h18v72H92V46L58 73 4 30V12Z" />
  <path d="M126 84V62l34-50h20l34 50v22h-20V68h-30l-12 16h-26Zm38-36h18l-9-16-9 16Z" />
  <path d="M242 12h52v20h-44v32h24V50h20v34h-52l-12-12V24l12-12Z" />
  <path d="M310 12h20v72h-20V12Z" />
</svg>

<style>
  .magi-wordmark {
    display: block;
    flex: 0 0 auto;
    color: var(--foreground);
    user-select: none;
  }
</style>

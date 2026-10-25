<script lang="ts">
  import { untrack, type Snippet } from 'svelte';

  let { expanded, children }: { expanded: boolean; children: Snippet } = $props();
  let mounted = $state(untrack(() => expanded));

  // Keep closing content only for the transition, then release expensive renderers.
  // Geometry belongs to CSS Grid; no scrollHeight measurement or JS height tween.
  $effect(() => {
    if (expanded) {
      mounted = true;
      return;
    }
    if (!mounted) return;
    const timer = setTimeout(() => { mounted = false; },
      window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : 260);
    return () => clearTimeout(timer);
  });
</script>

{#if mounted}
  <div class="disclosure" class:open={expanded} inert={!expanded} aria-hidden={!expanded}>
    <div class="disclosure-inner">{@render children()}</div>
  </div>
{/if}

<style>
  .disclosure {
    display: grid;
    min-width: 0;
    max-width: 100%;
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: 0fr;
    transition: grid-template-rows 260ms var(--magi-ease-ink);
  }
  .disclosure.open { grid-template-rows: 1fr; }
  .disclosure-inner { min-width: 0; min-height: 0; overflow: hidden; }
  @starting-style { .disclosure.open { grid-template-rows: 0fr; } }
  @media (prefers-reduced-motion: reduce) {
    .disclosure { transition: none; }
  }
</style>

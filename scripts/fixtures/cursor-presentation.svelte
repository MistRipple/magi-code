<script>
  import { onMount } from 'svelte';
  import AgentCursorOverlay from '../../web/src/components/tabs/AgentCursorOverlay.svelte';
  let signal = $state(null);
  let visible = $state(true);
  let sequence = 0;
  onMount(() => {
    window.cursorTest.subscribe(event => {
      if (event.type === 'visibility') { visible = event.visible; return; }
      if (event.type !== 'agent_cursor') return;
      signal = { sequence: ++sequence, visible: event.visible, point: { x: event.displayX, y: event.displayY },
        action: event.action, presentation: event.presentation };
    });
    window.__cursorReady = true;
  });
</script>
<AgentCursorOverlay anchorName="--test-guest" {signal} hostVisible={visible} onpresented={result => window.cursorTest.complete(result)} />

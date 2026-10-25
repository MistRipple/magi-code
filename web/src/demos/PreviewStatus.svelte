<script lang="ts">
  import type { DemoStatus } from './workbench-fixtures';
  let { status }: { status: DemoStatus | 'cancelled' } = $props();
  const label = $derived(({ success: '成功', running: '运行中', error: '失败', pending: '等待确认', cancelled: '已取消' })[status]);
</script>

<span class="status" class:green={status === 'success'} class:blue={status === 'running'} class:red={status === 'error'} class:amber={status === 'pending'} role="img" aria-label={label} title={label}></span>

<style>
  .status { display: inline-flex; flex: none; width: 7px; height: 7px; border-radius: 50%; background: currentColor; }
  .status::before { content: none; }
  .green { color: var(--success, #10b981); }
  .blue { color: var(--info, #3b82f6); animation: demo-status-pulse 1.5s ease-in-out infinite; }
  .red { color: var(--error, #ef4444); }
  .amber { color: var(--warning, #f59e0b); }
  .status:not(.green):not(.blue):not(.red):not(.amber) { color: var(--muted); }
  @keyframes demo-status-pulse { 50% { opacity: .3; } }
  @media (prefers-reduced-motion: reduce) { .blue { animation: none; } }
</style>

<script lang="ts">
  import Icon from './Icon.svelte';

  // 提示面板类型
  type CalloutType = 'info' | 'success' | 'warning' | 'error' | 'tip' | 'note';

  interface Props {
    type?: CalloutType;
    title?: string;
    icon?: string;
    closable?: boolean;
    collapsed?: boolean;
    onclose?: () => void;
    children?: import('svelte').Snippet;
  }

  let {
    type = 'info',
    title,
    icon,
    closable = false,
    collapsed = false,
    onclose,
    children
  }: Props = $props();

  // 内部折叠状态
  let isCollapsed = $state(collapsed);

  // 类型配置
  const typeConfig: Record<CalloutType, { icon: string; label: string }> = {
    info: { icon: 'info', label: '信息' },
    success: { icon: 'check', label: '成功' },
    warning: { icon: 'warning', label: '警告' },
    error: { icon: 'close', label: '错误' },
    tip: { icon: 'lightbulb', label: '提示' },
    note: { icon: 'note', label: '注意' }
  };

  const config = $derived(typeConfig[type] || typeConfig.info);
  const displayIcon = $derived(icon || config.icon);
  const displayTitle = $derived(title || config.label);

  function toggle() {
    isCollapsed = !isCollapsed;
  }

  function handleClose() {
    onclose?.();
  }
</script>

<div 
  class="callout callout--{type}"
  class:collapsed={isCollapsed}
  role="alert"
>
  <div class="callout-header" onclick={toggle}>
    <span class="callout-icon">
      <Icon name={displayIcon} size={16} />
    </span>
    <span class="callout-title">{displayTitle}</span>
    
    <div class="callout-actions">
      {#if closable}
        <button class="callout-close" onclick={handleClose} title="关闭">
          <Icon name="close" size={12} />
        </button>
      {/if}
      <span class="callout-chevron">
        <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
          <path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>
        </svg>
      </span>
    </div>
  </div>
  
  {#if !isCollapsed && children}
    <div class="callout-content">
      {@render children()}
    </div>
  {/if}
</div>

<style>
  .callout {
    border-radius: var(--radius-md);
    margin: var(--space-2) 0;
    overflow: hidden;
    border: 1px solid var(--border);
  }

  /* 类型颜色 */
  .callout--info { border-left: 3px solid var(--info); background: rgba(59, 130, 246, 0.08); }
  .callout--success { border-left: 3px solid var(--success); background: rgba(34, 197, 94, 0.08); }
  .callout--warning { border-left: 3px solid var(--warning); background: rgba(234, 179, 8, 0.08); }
  .callout--error { border-left: 3px solid var(--error); background: rgba(239, 68, 68, 0.08); }
  .callout--tip { border-left: 3px solid #a855f7; background: rgba(168, 85, 247, 0.08); }
  .callout--note { border-left: 3px solid #6366f1; background: rgba(99, 102, 241, 0.08); }

  .callout-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    user-select: none;
  }

  .callout-header:hover { background: rgba(255, 255, 255, 0.03); }

  .callout-icon { display: flex; align-items: center; }
  .callout--info .callout-icon { color: var(--info); }
  .callout--success .callout-icon { color: var(--success); }
  .callout--warning .callout-icon { color: var(--warning); }
  .callout--error .callout-icon { color: var(--error); }
  .callout--tip .callout-icon { color: #a855f7; }
  .callout--note .callout-icon { color: #6366f1; }

  .callout-title { flex: 1; font-weight: 500; font-size: var(--text-sm); }
  .callout-actions { display: flex; align-items: center; gap: var(--space-1); }
  .callout-close {
    padding: var(--space-1); background: transparent; border: none;
    color: var(--foreground-muted); cursor: pointer; border-radius: var(--radius-sm);
  }
  .callout-close:hover { background: var(--surface-hover); color: var(--foreground); }

  .callout-chevron {
    display: flex; transition: transform var(--transition-fast);
    color: var(--foreground-muted);
  }
  .callout:not(.collapsed) .callout-chevron { transform: rotate(90deg); }

  .callout-content {
    padding: var(--space-2) var(--space-3);
    border-top: 1px solid var(--border);
    font-size: var(--text-sm);
    line-height: 1.5;
    animation: expandCallout 0.2s ease-out;
  }

  @keyframes expandCallout {
    from { opacity: 0; transform: translateY(-4px); }
    to { opacity: 1; transform: translateY(0); }
  }
</style>


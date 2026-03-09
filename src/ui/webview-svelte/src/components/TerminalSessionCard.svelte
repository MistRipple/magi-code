<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import { getTerminalToolDisplayName, parseLeadingJson } from '../lib/terminal-utils';
  import { terminalSessions } from '../stores/terminal-sessions.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { ToolCall } from '../types/message';

  interface Props {
    toolCall?: ToolCall;
    status?: 'pending' | 'running' | 'success' | 'error';
    initialExpanded?: boolean;
  }

  let { toolCall, status = 'running', initialExpanded = true }: Props = $props();
  let collapsed = $state(untrack(() => !initialExpanded));

  function parseJson(content?: string): Record<string, unknown> | null {
    const parsed = parseLeadingJson(content);
    if (!parsed || Array.isArray(parsed)) return null;
    return parsed as Record<string, unknown>;
  }

  function formatOutput(content: string): string {
    const trimmed = content.trim();
    if (!trimmed) return '';
    if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
      try {
        return JSON.stringify(JSON.parse(trimmed), null, 2);
      } catch {
        // 非 JSON 或不完整 JSON，保持原始输出
      }
    }
    return content;
  }


  function toggle(): void {
    collapsed = !collapsed;
  }

  const parsedResult = $derived(parseJson(toolCall?.result));
  const terminalId = $derived.by(() => {
    const fromResult = parsedResult?.terminal_id;
    if (Number.isInteger(fromResult)) return fromResult as number;
    const fromArgs = toolCall?.arguments?.terminal_id;
    if (Number.isInteger(fromArgs)) return fromArgs as number;
    return undefined;
  });

  $effect(() => {
    if (toolCall) {
      terminalSessions.ingestToolCall(toolCall);
    }
  });

  const session = $derived.by(() => {
    if (terminalId) {
      return terminalSessions.getById(terminalId);
    }
    return terminalSessions.getByToolCallId(toolCall?.id);
  });

  const displayStatus = $derived(session?.status || status || toolCall?.status || 'running');
  const displayPhase = $derived(session?.phase || (typeof parsedResult?.phase === 'string' ? parsedResult.phase : ''));
  const displayMode = $derived.by(() => {
    if (session?.runMode) return session.runMode;
    const raw = parsedResult?.run_mode;
    if (raw === 'service' || raw === 'task') return raw;
    return '';
  });
  const displayCommand = $derived(session?.command || (typeof toolCall?.arguments?.command === 'string' ? toolCall.arguments.command : ''));
  const displayCwd = $derived(session?.cwd || (typeof parsedResult?.cwd === 'string' ? parsedResult.cwd : ''));
  const fallbackOutput = $derived.by(() => {
    if (typeof parsedResult?.output === 'string') return parsedResult.output;
    if (typeof parsedResult?.final_output === 'string') return parsedResult.final_output;
    return '';
  });
  const displayOutput = $derived(formatOutput(session?.output || fallbackOutput || ''));
  const outputCursor = $derived(session?.outputCursor);
  const returnCode = $derived(session?.returnCode);
  const locked = $derived(Boolean(session?.locked));
  const startupMessage = $derived(session?.startupMessage || '');
  const errorText = $derived(session?.error || toolCall?.error || '');
  const accepted = $derived(session?.accepted);
  const killed = $derived(session?.killed);
  const releasedLock = $derived(session?.releasedLock);
  const showOutput = $derived(displayOutput.trim().length > 0);

  const normalizedStatus = $derived(String(displayStatus || '').toLowerCase());

  const isStreaming = $derived(
    normalizedStatus === 'running' || normalizedStatus === 'starting' || normalizedStatus === 'pending'
  );

  const statusClass = $derived.by(() => {
    if (normalizedStatus === 'failed' || normalizedStatus === 'error' || normalizedStatus === 'timeout' || normalizedStatus === 'killed') return 'error';
    if (normalizedStatus === 'completed' || normalizedStatus === 'success' || normalizedStatus === 'ready') return 'success';
    if (normalizedStatus === 'pending' || normalizedStatus === 'starting') return 'pending';
    return 'running';
  });

  const showStatusPulse = $derived(statusClass === 'running' || statusClass === 'pending');
  const titleText = $derived(i18n.t('terminalSession.title', { id: terminalId ?? '-' }));
  const toolNameLabel = $derived(getTerminalToolDisplayName(toolCall?.name));
  const toolSummary = $derived(displayCommand?.trim() || '');

  const isExpandable = $derived(Boolean(
    displayCommand
    || displayCwd
    || showOutput
    || startupMessage
    || errorText
    || typeof outputCursor === 'number'
    || typeof returnCode === 'number'
  ));
  const isExpanded = $derived(isExpandable && !collapsed);

  let outputElement = $state<HTMLPreElement | null>(null);
  let followTail = $state(true);

  function nearTail(element: HTMLPreElement): boolean {
    const threshold = 24;
    return element.scrollHeight - element.scrollTop - element.clientHeight <= threshold;
  }

  function handleOutputScroll(): void {
    if (!outputElement) return;
    followTail = nearTail(outputElement);
  }

  $effect(() => {
    if (!outputElement || !showOutput || !isExpanded) return;
    if (followTail || isStreaming) {
      queueMicrotask(() => {
        if (!outputElement) return;
        outputElement.scrollTop = outputElement.scrollHeight;
      });
    }
  });
</script>

{#if isExpandable}
  <div class="tool-call terminal-call" class:collapsed={collapsed} data-status={statusClass}>
    <button class="tool-header" onclick={toggle}>
      <span class="chevron">
        <Icon name="chevron-right" size={12} />
      </span>
      <span class="tool-icon"><Icon name="terminal" size={14} /></span>
      <span class="tool-title">
        <span class="tool-name">{toolNameLabel}</span>
        <span class="tool-summary" title={toolSummary || titleText}>{toolSummary || titleText}</span>
      </span>
      <span class="tool-status status-{statusClass}">
        {#if showStatusPulse}
          <span class="status-dot pulsing"></span>
        {:else}
          <span class="status-dot"></span>
        {/if}
      </span>
      <span class="terminal-stream-indicator" data-active={isStreaming}>
        {isStreaming ? 'LIVE' : 'DONE'}
      </span>
    </button>

    {#if !collapsed}
      <div class="tool-content terminal-content">
        <div class="terminal-meta-grid">
          <div class="terminal-meta-item">{i18n.t('terminalSession.title', { id: terminalId ?? '-' })}</div>
          <div class="terminal-meta-item">{i18n.t('terminalSession.status')}: {displayStatus}</div>
          {#if displayMode}
            <div class="terminal-meta-item">{i18n.t('terminalSession.mode')}: {displayMode}</div>
          {/if}
          {#if displayPhase}
            <div class="terminal-meta-item">{i18n.t('terminalSession.phase')}: {displayPhase}</div>
          {/if}
        </div>

        {#if displayCommand}
          <div class="terminal-meta">{i18n.t('terminalSession.command')}: <code>{displayCommand}</code></div>
        {/if}
        {#if displayCwd}
          <div class="terminal-meta">{i18n.t('terminalSession.cwd')}: <code>{displayCwd}</code></div>
        {/if}

        <div class="terminal-section-label">{i18n.t('terminalSession.output')}</div>
        {#if showOutput}
          <pre
            class="terminal-output"
            bind:this={outputElement}
            onscroll={handleOutputScroll}
          >{displayOutput}</pre>
        {:else}
          <pre class="terminal-output terminal-empty">{i18n.t('terminalSession.noOutput')}</pre>
        {/if}

        {#if startupMessage}
          <div class="terminal-hint">{i18n.t('terminalSession.startup')}: {startupMessage}</div>
        {/if}

        {#if errorText}
          <div class="terminal-error">{i18n.t('terminalSession.error')}: {errorText}</div>
        {/if}

        <div class="terminal-footer">
          {#if typeof outputCursor === 'number'}
            <span>{i18n.t('terminalSession.cursor')}: {outputCursor}</span>
          {/if}
          {#if typeof returnCode === 'number'}
            <span>{i18n.t('terminalSession.returnCode')}: {returnCode}</span>
          {/if}
          <span>{i18n.t('terminalSession.locked')}: {locked ? i18n.t('terminalSession.yes') : i18n.t('terminalSession.no')}</span>
          {#if typeof accepted === 'boolean'}
            <span>{i18n.t('terminalSession.accepted')}: {accepted ? i18n.t('terminalSession.yes') : i18n.t('terminalSession.no')}</span>
          {/if}
          {#if typeof killed === 'boolean'}
            <span>{i18n.t('terminalSession.killed')}: {killed ? i18n.t('terminalSession.yes') : i18n.t('terminalSession.no')}</span>
          {/if}
          {#if typeof releasedLock === 'boolean'}
            <span>{i18n.t('terminalSession.releasedLock')}: {releasedLock ? i18n.t('terminalSession.yes') : i18n.t('terminalSession.no')}</span>
          {/if}
        </div>
      </div>
    {/if}
  </div>
{:else}
  <div class="tool-call terminal-call" data-status={statusClass}>
    <div class="tool-header terminal-header-flat">
      <span class="tool-icon"><Icon name="terminal" size={14} /></span>
      <span class="tool-title">
        <span class="tool-name">{toolNameLabel}</span>
        <span class="tool-summary" title={titleText}>{titleText}</span>
      </span>
      <span class="tool-status status-{statusClass}">
        {#if showStatusPulse}
          <span class="status-dot pulsing"></span>
        {:else}
          <span class="status-dot"></span>
        {/if}
      </span>
      <span class="terminal-stream-indicator" data-active={isStreaming}>
        {isStreaming ? 'LIVE' : 'DONE'}
      </span>
    </div>
  </div>
{/if}

<style>
  .tool-call {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--space-2, 8px) 0;
    overflow: hidden;
    background: var(--surface-1, rgba(255, 255, 255, 0.02));
  }

  .tool-header {
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    width: 100%;
    padding: var(--space-2, 8px) var(--space-3, 12px);
    background: transparent;
    border: none;
    text-align: left;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .tool-header:hover {
    background: var(--surface-hover, rgba(255, 255, 255, 0.05));
  }

  .terminal-header-flat {
    cursor: default;
  }

  .terminal-header-flat:hover {
    background: transparent;
  }

  .chevron {
    display: flex;
    color: var(--foreground-muted);
    transition: transform var(--transition-fast);
  }

  .collapsed .chevron {
    transform: rotate(0deg);
  }

  .tool-call:not(.collapsed) .chevron {
    transform: rotate(90deg);
  }

  .tool-icon {
    display: flex;
    color: var(--info);
  }

  .tool-title {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    min-width: 0;
    overflow: hidden;
  }

  .tool-name {
    font-weight: 500;
    font-size: var(--text-sm, 13px);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .tool-summary {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    opacity: 0.8;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 1;
  }

  .tool-status {
    display: flex;
    align-items: center;
    flex-shrink: 0;
  }

  .status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }

  .status-dot.pulsing {
    animation: pulse 1.5s ease-in-out infinite;
  }

  .status-pending {
    color: var(--warning);
  }

  .status-running {
    color: var(--info);
  }

  .status-success {
    color: var(--success);
  }

  .status-error {
    color: var(--error);
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.3;
    }
  }

  .tool-content {
    padding: var(--space-3, 12px);
    border-top: 1px solid var(--border);
    background: var(--surface-2, rgba(0, 0, 0, 0.1));
    animation: slideDown 0.2s ease-out;
    transform-origin: top;
  }

  @keyframes slideDown {
    from {
      opacity: 0;
      max-height: 0;
      transform: translateY(-8px);
    }
    to {
      opacity: 1;
      max-height: 700px;
      transform: translateY(0);
    }
  }

  .terminal-stream-indicator {
    font-size: 10px;
    line-height: 1;
    font-weight: 700;
    letter-spacing: 0.4px;
    border-radius: 999px;
    padding: 2px 7px;
    border: 1px solid rgba(16, 185, 129, 0.45);
    color: #34d399;
    background: rgba(16, 185, 129, 0.12);
    flex-shrink: 0;
  }

  .terminal-stream-indicator[data-active='true'] {
    color: #60a5fa;
    border-color: rgba(96, 165, 250, 0.55);
    background: rgba(59, 130, 246, 0.16);
    box-shadow: 0 0 0 1px rgba(59, 130, 246, 0.2) inset;
  }

  .terminal-meta-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 6px;
  }

  .terminal-meta-item {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
  }

  .terminal-meta {
    padding: 6px 0 0 0;
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
  }

  .terminal-section-label {
    padding: 8px 0 4px 0;
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.4px;
  }

  .terminal-output {
    margin: 0 0 8px 0;
    background: #0b0f14;
    color: #d7e3f4;
    border-radius: var(--radius-sm);
    border: 1px solid rgba(132, 156, 182, 0.2);
    padding: 10px;
    max-height: 320px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-size: 12px;
    line-height: 1.45;
    font-family: var(--font-mono);
  }

  .terminal-empty {
    color: #8ea0b6;
  }

  .terminal-hint {
    margin: 0 0 8px 0;
    font-size: 11px;
    color: var(--warning);
  }

  .terminal-error {
    margin: 0 0 8px 0;
    font-size: 11px;
    color: var(--error);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .terminal-footer {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    padding: 0 0 2px 0;
    font-size: 11px;
    color: var(--foreground-muted);
  }

  .terminal-call[data-status='error'] {
    border-color: var(--error);
  }

  .terminal-call[data-status='success'] {
    border-color: rgba(16, 185, 129, 0.45);
  }

  .terminal-call[data-status='running'] {
    border-color: rgba(59, 130, 246, 0.45);
  }
</style>

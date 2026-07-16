<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import AccessProfileSwitchAction from './AccessProfileSwitchAction.svelte';
  import {
    parseLeadingJson,
    resolveTerminalArgumentId,
    terminalPayloadErrorText,
    terminalPayloadOutput,
  } from '../lib/terminal-utils';
  import { i18n } from '../stores/i18n.svelte';
  import type { ToolCall, TerminalSessionBlock } from '../types/message';
  import {
    isAccessModeApprovalErrorPayload,
    isStructuredToolErrorPayload,
    publicToolPayloadMessage,
    toolPayloadStatus,
  } from '../lib/tool-error-payload';

  interface Props {
    toolCall?: ToolCall;
    status?: 'pending' | 'running' | 'success' | 'error';
  }

  let { toolCall, status = 'running' }: Props = $props();
  let collapsed = $state(untrack(() => !(status === 'running' || status === 'pending')));
  let lastStatusClass = $state(untrack(() => status));
  let userToggled = $state(false);

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

  function readInt(value: unknown): number | undefined {
    return Number.isInteger(value) ? value as number : undefined;
  }

  function readNullableInt(value: unknown): number | null | undefined {
    if (typeof value === 'number' && Number.isFinite(value)) return value;
    return value === null ? null : undefined;
  }

  function readBool(value: unknown): boolean | undefined {
    return typeof value === 'boolean' ? value : undefined;
  }

  function readString(value: unknown): string | undefined {
    return typeof value === 'string' ? value : undefined;
  }

  function terminalStatusFromCanonical(
    canonicalStatus?: 'pending' | 'running' | 'success' | 'error',
    payloadStatus?: string,
  ): string {
    if (canonicalStatus === 'error') {
      const normalizedPayloadStatus = (payloadStatus || '').trim().toLowerCase();
      if (
        normalizedPayloadStatus
        && normalizedPayloadStatus !== 'succeeded'
        && normalizedPayloadStatus !== 'success'
        && normalizedPayloadStatus !== 'completed'
        && normalizedPayloadStatus !== 'complete'
        && normalizedPayloadStatus !== 'done'
      ) {
        return payloadStatus || 'error';
      }
      return 'error';
    }
    if (canonicalStatus === 'running' || canonicalStatus === 'pending') {
      return canonicalStatus;
    }
    return payloadStatus || canonicalStatus || 'running';
  }

  function toggle(): void {
    if (!canToggle) {
      return;
    }
    collapsed = !collapsed;
    userToggled = true;
  }

  const parsedResult = $derived(parseJson(toolCall?.result));
  const parsedErrorResult = $derived(parseJson(toolCall?.error));
  const terminalPayload = $derived(parsedResult || parsedErrorResult);
  const terminal = $derived.by((): Partial<TerminalSessionBlock> | undefined => {
    if (!terminalPayload && !toolCall) {
      return undefined;
    }
    const rawMode = terminalPayload?.run_mode;
    const runMode = rawMode === 'service' || rawMode === 'task' ? rawMode : undefined;
    const payloadTerminalId = readInt(terminalPayload?.terminal_id);
    return {
      terminalId: payloadTerminalId && payloadTerminalId > 0
        ? payloadTerminalId
        : resolveTerminalArgumentId(toolCall?.arguments),
      status: readString(terminalPayload?.status) || undefined,
      phase: readString(terminalPayload?.phase),
      runMode,
      terminalName: readString(terminalPayload?.terminal_name),
      cwd: readString(terminalPayload?.cwd),
      command: readString(terminalPayload?.command)
        ?? (typeof toolCall?.arguments?.command === 'string' ? toolCall.arguments.command : undefined),
      output: terminalPayloadOutput(terminalPayload) || undefined,
      outputCursor: readInt(terminalPayload?.output_cursor),
      outputStartCursor: readInt(terminalPayload?.output_start_cursor),
      fromCursor: readInt(terminalPayload?.from_cursor),
      nextCursor: readInt(terminalPayload?.next_cursor),
      delta: readBool(terminalPayload?.delta),
      truncated: readBool(terminalPayload?.truncated),
      startupStatus: terminalPayload?.startup_status === 'pending'
        || terminalPayload?.startup_status === 'confirmed'
        || terminalPayload?.startup_status === 'timeout'
        || terminalPayload?.startup_status === 'failed'
        || terminalPayload?.startup_status === 'skipped'
        ? terminalPayload.startup_status as TerminalSessionBlock['startupStatus']
        : undefined,
      startupMessage: readString(terminalPayload?.startup_message),
      locked: readBool(terminalPayload?.locked),
      returnCode: readNullableInt(terminalPayload?.return_code) ?? readNullableInt(terminalPayload?.exit_code),
      accepted: readBool(terminalPayload?.accepted),
      killed: readBool(terminalPayload?.killed),
      releasedLock: readBool(terminalPayload?.released_lock),
      error: terminalPayloadErrorText(terminalPayload) || undefined,
    };
  });
  const terminalId = $derived(terminal?.terminalId);
  const errorPayloadStatus = $derived(toolPayloadStatus(parsedErrorResult));
  const displayStatus = $derived(terminalStatusFromCanonical(
    status || toolCall?.status,
    terminal?.status || errorPayloadStatus,
  ));
  const displayPhase = $derived(terminal?.phase || '');
  const displayMode = $derived(terminal?.runMode || '');
  const displayCommand = $derived(terminal?.command || '');
  const displayCwd = $derived(terminal?.cwd || '');
  const displayOutput = $derived.by(() => {
    const fromTerminal = terminal?.output;
    if (typeof fromTerminal === 'string' && fromTerminal.length > 0) {
      if (isStructuredToolErrorPayload(fromTerminal)) {
        return '';
      }
      return formatOutput(fromTerminal);
    }
    if (parsedResult) {
      return '';
    }
    const raw = typeof toolCall?.result === 'string' ? toolCall.result : '';
    if (isStructuredToolErrorPayload(raw)) {
      return '';
    }
    return formatOutput(raw);
  });

  function normalizeDisplayText(value: string): string {
    return value.trim().replace(/\s+/g, ' ');
  }

  function terminalStatusLabel(value: string, classValue: string): string {
    const normalized = value.trim().toLowerCase();
    if (['pending', 'queued', 'starting'].includes(normalized)) {
      return i18n.t('terminalSession.status.pending');
    }
    if (['running', 'in_progress', 'active'].includes(normalized) || normalized.includes('running')) {
      return i18n.t('terminalSession.status.running');
    }
    if (
      ['succeeded', 'success', 'completed', 'complete', 'done'].includes(normalized)
      || normalized.includes('success')
    ) {
      return i18n.t('terminalSession.status.success');
    }
    if (
      ['failed', 'error', 'timeout', 'cancelled', 'canceled', 'aborted', 'rejected'].includes(normalized)
      || normalized.includes('fail')
      || normalized.includes('error')
      || normalized.includes('timeout')
      || normalized.includes('cancel')
      || normalized.includes('abort')
      || normalized.includes('reject')
    ) {
      return i18n.t('terminalSession.status.error');
    }
    if (classValue === 'success') return i18n.t('terminalSession.status.success');
    if (classValue === 'error') return i18n.t('terminalSession.status.error');
    if (classValue === 'pending') return i18n.t('terminalSession.status.pending');
    return i18n.t('terminalSession.status.running');
  }

  function terminalModeLabel(value: string): string {
    const normalized = value.trim().toLowerCase();
    if (normalized === 'task') return i18n.t('terminalSession.mode.task');
    if (normalized === 'service') return i18n.t('terminalSession.mode.service');
    return i18n.t('terminalSession.mode.default');
  }

  function terminalPhaseLabel(value: string): string {
    const normalized = value.trim().toLowerCase();
    if (!normalized) return '';
    if (normalized.includes('start') || normalized.includes('init')) {
      return i18n.t('terminalSession.phase.starting');
    }
    if (normalized.includes('wait') || normalized.includes('approval') || normalized.includes('pending')) {
      return i18n.t('terminalSession.phase.waiting');
    }
    if (normalized.includes('finish') || normalized.includes('complete') || normalized.includes('success')) {
      return i18n.t('terminalSession.phase.completed');
    }
    if (
      normalized.includes('fail')
      || normalized.includes('error')
      || normalized.includes('timeout')
      || normalized.includes('cancel')
    ) {
      return i18n.t('terminalSession.phase.interrupted');
    }
    return i18n.t('terminalSession.phase.running');
  }

  const outputCursor = $derived(terminal?.outputCursor);
  const returnCode = $derived(terminal?.returnCode);
  const locked = $derived(terminal?.locked);
  const startupMessage = $derived(terminal?.startupMessage || '');
  const publicErrorText = $derived(
    publicToolPayloadMessage(parsedErrorResult)
    || publicToolPayloadMessage(parsedResult)
    || publicToolPayloadMessage(terminal?.output)
    || publicToolPayloadMessage(toolCall?.error)
    || publicToolPayloadMessage(toolCall?.result)
  );
  const shouldOfferFullAccessSwitch = $derived(
    isAccessModeApprovalErrorPayload(parsedErrorResult)
    || isAccessModeApprovalErrorPayload(parsedResult)
    || isAccessModeApprovalErrorPayload(terminal?.output)
    || isAccessModeApprovalErrorPayload(toolCall?.error)
    || isAccessModeApprovalErrorPayload(toolCall?.result)
  );
  const errorText = $derived(
    terminal?.error
    || publicToolPayloadMessage(terminal?.output)
    || terminalPayloadErrorText(parsedErrorResult)
    || publicToolPayloadMessage(toolCall?.error)
    || toolCall?.error
    || ''
  );
  const showErrorHint = $derived.by(() => {
    const normalizedError = normalizeDisplayText(publicErrorText || errorText);
    if (!normalizedError) {
      return false;
    }
    return !normalizeDisplayText(displayOutput).includes(normalizedError);
  });
  const accepted = $derived(terminal?.accepted);
  const killed = $derived(terminal?.killed);
  const releasedLock = $derived(terminal?.releasedLock);
  const showOutput = $derived(displayOutput.trim().length > 0);

  const normalizedStatus = $derived(String(displayStatus || '').toLowerCase());

  const statusClass = $derived.by(() => {
    if (
      normalizedStatus.includes('fail')
      || normalizedStatus.includes('error')
      || normalizedStatus.includes('timeout')
      || normalizedStatus.includes('kill')
      || normalizedStatus.includes('reject')
      || normalizedStatus.includes('block')
      || normalizedStatus.includes('approval')
      || normalizedStatus.includes('cancel')
      || normalizedStatus.includes('abort')
    ) return 'error';
    if (
      normalizedStatus.includes('complete')
      || normalizedStatus.includes('success')
      || normalizedStatus.includes('succeed')
      || normalizedStatus === 'ready'
      || normalizedStatus === 'done'
    ) return 'success';
    if (normalizedStatus.includes('pending') || normalizedStatus.includes('starting')) return 'pending';
    return 'running';
  });
  const displayStatusLabel = $derived(terminalStatusLabel(displayStatus, statusClass));
  const displayModeLabel = $derived(displayMode ? terminalModeLabel(displayMode) : '');
  const displayPhaseLabel = $derived(displayPhase ? terminalPhaseLabel(displayPhase) : '');

  const showStatusPulse = $derived(statusClass === 'running' || statusClass === 'pending');
  const isActiveStatus = $derived(statusClass === 'running' || statusClass === 'pending');

  $effect(() => {
    if (statusClass === lastStatusClass) {
      return;
    }
    lastStatusClass = statusClass;
    if (isActiveStatus) {
      if (!userToggled) {
        collapsed = false;
      }
      return;
    }
    if (!userToggled) {
      collapsed = true;
    }
  });
  const toolNameLabel = $derived(i18n.t('toolCall.displayName.shell'));
  const titleText = $derived(
    typeof terminalId === 'number'
      ? i18n.t('terminalSession.title', { id: terminalId })
      : toolNameLabel
  );
  const toolSummary = $derived(displayCommand?.trim() || '');

  const isExpandable = $derived(Boolean(
    displayCommand
    || displayCwd
    || showOutput
    || startupMessage
    || showErrorHint
    || typeof outputCursor === 'number'
    || typeof returnCode === 'number'
    || typeof locked === 'boolean'
    || typeof accepted === 'boolean'
    || typeof killed === 'boolean'
    || typeof releasedLock === 'boolean'
  ));
  const canToggle = $derived(isExpandable);
  const isExpanded = $derived(canToggle && !collapsed);
  const showFooter = $derived(
    typeof outputCursor === 'number'
    || typeof returnCode === 'number'
    || typeof locked === 'boolean'
    || typeof accepted === 'boolean'
    || typeof killed === 'boolean'
    || typeof releasedLock === 'boolean'
  );

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
    if (followTail || showStatusPulse) {
      queueMicrotask(() => {
        if (!outputElement) return;
        outputElement.scrollTop = outputElement.scrollHeight;
      });
    }
  });
</script>

{#if isExpandable}
  <div
    class="tool-call terminal-call"
    class:collapsed={canToggle && collapsed}
    data-status={statusClass}
    data-tool-name={toolCall?.name || ''}
    data-terminal-id={typeof terminalId === 'number' ? String(terminalId) : undefined}
  >
    {#if canToggle}
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
      </button>
    {:else}
      <div class="tool-header terminal-header-flat">
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
      </div>
    {/if}

    {#if canToggle && !collapsed}
      <div class="tool-content terminal-content">
        <div class="terminal-meta-grid">
          {#if typeof terminalId === 'number'}
            <div class="terminal-meta-item">{i18n.t('terminalSession.title', { id: terminalId })}</div>
          {/if}
          <div class="terminal-meta-item">{i18n.t('terminalSession.status')}: {displayStatusLabel}</div>
          {#if displayModeLabel}
            <div class="terminal-meta-item">{i18n.t('terminalSession.mode')}: {displayModeLabel}</div>
          {/if}
          {#if displayPhaseLabel}
            <div class="terminal-meta-item">{i18n.t('terminalSession.phase')}: {displayPhaseLabel}</div>
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

        {#if showErrorHint}
          <div class="terminal-error">{i18n.t('terminalSession.error')}: {publicErrorText || errorText || i18n.t('terminalSession.errorHint')}</div>
        {/if}
        {#if shouldOfferFullAccessSwitch}
          <div class="terminal-hint">{i18n.t('toolCall.errorDiagnosis.permission.hint')}</div>
          <AccessProfileSwitchAction />
        {/if}

        {#if showFooter}
          <div class="terminal-footer">
            {#if typeof outputCursor === 'number'}
              <span>{i18n.t('terminalSession.cursor')}: {outputCursor}</span>
            {/if}
            {#if typeof returnCode === 'number'}
              <span>{i18n.t('terminalSession.returnCode')}: {returnCode}</span>
            {/if}
            {#if typeof locked === 'boolean'}
              <span>{i18n.t('terminalSession.locked')}: {locked ? i18n.t('terminalSession.yes') : i18n.t('terminalSession.no')}</span>
            {/if}
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
        {/if}
      </div>
    {/if}
  </div>
{:else}
  <div
    class="tool-call terminal-call"
    data-status={statusClass}
    data-tool-name={toolCall?.name || ''}
    data-terminal-id={typeof terminalId === 'number' ? String(terminalId) : undefined}
  >
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
    </div>
  </div>
{/if}

<style>
  .tool-call {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin-top: var(--space-2);
    overflow: hidden;
    background: var(--surface-1);
  }

  /* header 高度/padding/字号/accent 条/chevron 等共享规范见 styles/tool-card.css；
     terminal-header-flat 是不可点击的扁平模式，需要覆盖共享 cursor 与 hover */
  .terminal-header-flat {
    cursor: default;
  }

  .terminal-header-flat:hover {
    background: transparent;
  }

  /* tool-icon 中性化：accent 条承担状态色，图标用 muted 避免三层颜色冲突 */
  .tool-icon {
    display: flex;
    color: var(--foreground-muted);
  }

  .tool-title {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
    overflow: hidden;
  }

  .tool-summary {
    font-size: var(--text-xs);
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
    padding: var(--space-3);
    border-top: 1px solid var(--border);
    background: var(--surface-2);
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

  .terminal-meta-grid {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
  }

  .terminal-meta-item {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .terminal-meta {
    padding: var(--space-2) 0 0 0;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .terminal-section-label {
    padding-top: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.4px;
  }

  .terminal-output {
    margin-top: var(--space-2);
    background: #0b0f14;
    color: #d7e3f4;
    border-radius: var(--radius-sm);
    border: 1px solid rgba(132, 156, 182, 0.2);
    padding: var(--space-4);
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
    margin-top: var(--space-3);
    font-size: var(--text-xs);
    color: var(--warning);
  }

  .terminal-error {
    margin-top: var(--space-3);
    font-size: var(--text-xs);
    color: var(--error);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .terminal-footer {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
    padding-top: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .terminal-call[data-status='error'] {
    border-color: var(--error);
  }
</style>

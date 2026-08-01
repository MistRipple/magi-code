<script lang="ts">
  import type { ModelFailureDiagnostic } from '../lib/model-failure';
  import type { ToolCallFailureDiagnostic } from '../lib/tool-call-failure';
  import { i18n } from '../stores/i18n.svelte';
  import { onDestroy } from 'svelte';
  import Icon from './Icon.svelte';

  interface Props {
    failure: ModelFailureDiagnostic | ToolCallFailureDiagnostic;
  }

  let { failure }: Props = $props();
  let copied = $state(false);
  let copyResetTimer: ReturnType<typeof setTimeout> | null = null;

  const isToolCallFailure = $derived(failure.schemaVersion === 'tool-call-failure.v1');
  const isUnavailableTool = $derived(
    isToolCallFailure
      && (failure as ToolCallFailureDiagnostic).reasonCode === 'tool_not_available',
  );
  const title = $derived(i18n.t(isUnavailableTool
    ? 'messageItem.toolCallFailure.unavailableTitle'
    : isToolCallFailure
      ? 'messageItem.toolCallFailure.title'
      : 'messageItem.modelFailure.title'));

  const stageLabel = $derived.by(() => {
    switch (failure.stage) {
      case 'tool_call_validation':
        return i18n.t(isUnavailableTool
          ? 'messageItem.toolCallFailure.stage.toolAvailability'
          : 'messageItem.toolCallFailure.stage.toolCallValidation');
      case 'model_configuration':
        return i18n.t('messageItem.modelFailure.stage.modelConfiguration');
      case 'request_dispatch':
        return i18n.t('messageItem.modelFailure.stage.requestDispatch');
      case 'response_stream':
        return i18n.t('messageItem.modelFailure.stage.responseStream');
      case 'response_stream_recovery':
        return i18n.t('messageItem.modelFailure.stage.responseStreamRecovery');
      case 'response_validation':
        return i18n.t('messageItem.modelFailure.stage.responseValidation');
      case 'response_finalization':
        return i18n.t('messageItem.modelFailure.stage.responseFinalization');
      default:
        return failure.stage;
    }
  });

  const recoveryText = $derived.by(() => {
    if (isToolCallFailure) {
      return i18n.t(isUnavailableTool
        ? 'messageItem.toolCallFailure.unavailableRecoveryAttempts'
        : 'messageItem.toolCallFailure.recoveryAttempts', {
        count: failure.retryAttempts,
      });
    }
    if (failure.retryAttempts > 0) {
      return i18n.t('messageItem.modelFailure.recoveryAttempts', {
        count: failure.retryAttempts,
      });
    }
    return (failure as ModelFailureDiagnostic).retryable
      ? i18n.t('messageItem.modelFailure.retryable')
      : i18n.t('messageItem.modelFailure.actionRequired');
  });

  async function copyFailure(): Promise<void> {
    const text = [
      title,
      `${i18n.t('messageItem.modelFailure.reason')}: ${failure.summary}`,
      ...(isToolCallFailure
        ? [`${i18n.t('messageItem.toolCallFailure.tool')}: ${(failure as ToolCallFailureDiagnostic).toolName}`]
        : []),
      `${i18n.t('messageItem.modelFailure.stage')}: ${stageLabel}`,
      `${i18n.t('messageItem.modelFailure.recovery')}: ${recoveryText}`,
      `${i18n.t('messageItem.modelFailure.code')}: ${failure.code}`,
      `${i18n.t('messageItem.modelFailure.diagnostic')}: ${failure.detail}`,
    ].join('\n');
    await navigator.clipboard.writeText(text);
    copied = true;
    if (copyResetTimer) clearTimeout(copyResetTimer);
    copyResetTimer = setTimeout(() => {
      copied = false;
      copyResetTimer = null;
    }, 1500);
  }

  onDestroy(() => {
    if (copyResetTimer) clearTimeout(copyResetTimer);
  });
</script>

<section class="model-failure" data-model-failure-code={failure.code}>
  <header class="model-failure-header">
    <span class="model-failure-title">
      <Icon name="alert-triangle" size={16} />
      {title}
    </span>
    <button
      type="button"
      class="model-failure-copy"
      onclick={() => void copyFailure()}
      title={copied
        ? i18n.t('messageItem.modelFailure.copied')
        : i18n.t('messageItem.modelFailure.copy')}
    >
      <Icon name={copied ? 'check' : 'copy'} size={14} />
    </button>
  </header>

  <p class="model-failure-summary">{failure.summary}</p>

  <dl class="model-failure-facts">
    {#if isToolCallFailure}
      <div>
        <dt>{i18n.t('messageItem.toolCallFailure.tool')}</dt>
        <dd><code>{(failure as ToolCallFailureDiagnostic).toolName}</code></dd>
      </div>
    {/if}
    <div>
      <dt>{i18n.t('messageItem.modelFailure.stage')}</dt>
      <dd>{stageLabel}</dd>
    </div>
    <div>
      <dt>{i18n.t('messageItem.modelFailure.recovery')}</dt>
      <dd>{recoveryText}</dd>
    </div>
    <div>
      <dt>{i18n.t('messageItem.modelFailure.code')}</dt>
      <dd><code>{failure.code}</code></dd>
    </div>
  </dl>

  <div class="model-failure-diagnostic">
    <span>{i18n.t('messageItem.modelFailure.diagnostic')}</span>
    <pre>{failure.detail}</pre>
  </div>
</section>

<style>
  .model-failure {
    width: 100%;
    max-width: none;
    box-sizing: border-box;
    padding: var(--space-3) var(--space-4);
    border: 1px solid color-mix(in srgb, var(--error) 42%, var(--border));
    border-left: 3px solid var(--error);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--error) 7%, var(--surface));
    color: var(--foreground);
    text-align: left;
  }

  .model-failure-header,
  .model-failure-title,
  .model-failure-copy,
  .model-failure-facts > div {
    display: flex;
    align-items: center;
  }

  .model-failure-header {
    justify-content: flex-start;
    gap: var(--space-2);
  }

  .model-failure-title {
    gap: var(--space-2);
    color: var(--error);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .model-failure-copy {
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .model-failure-copy:hover,
  .model-failure-copy:focus-visible {
    background: var(--background-hover);
    color: var(--foreground);
  }

  .model-failure-summary {
    margin: var(--space-2) 0 0;
    font-size: var(--text-base);
    line-height: var(--leading-relaxed);
    font-weight: var(--font-medium);
  }

  .model-failure-facts {
    display: grid;
    gap: 6px;
    margin: var(--space-3) 0 0;
    font-size: var(--text-sm);
  }

  .model-failure-facts > div {
    align-items: baseline;
    gap: var(--space-2);
    min-width: 0;
  }

  .model-failure-facts dt {
    flex: 0 0 72px;
    color: var(--foreground-muted);
  }

  .model-failure-facts dd {
    min-width: 0;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .model-failure-facts code {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }

  .model-failure-diagnostic {
    margin-top: var(--space-3);
  }

  .model-failure-diagnostic > span {
    display: block;
    margin-bottom: 4px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .model-failure-diagnostic pre {
    max-height: 240px;
    margin: 0;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.55;
    color: var(--foreground);
  }

  @media (max-width: 640px) {
    .model-failure {
      padding: var(--space-3);
    }

    .model-failure-facts dt {
      flex-basis: 64px;
    }
  }
</style>

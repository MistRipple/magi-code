<script lang="ts">
  import { untrack } from 'svelte';
  import type { ExecutionSteps, ExecutionStepStatus } from '../shared/app-server-protocol.generated';
  import type { CanonicalTurnItem } from '../shared/protocol/canonical-turn';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { IconName } from '../lib/icons';
  import { executionStepEvidence } from '../lib/execution-steps';
  import { turnStoreState } from '../stores/turn-store.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getAgentMessageItem } from '../web/agent-api';
  import { valueToDisplayText } from '../stores/turn-projection';
  import Icon from './Icon.svelte';
  import ToolCardMeta from './ToolCardMeta.svelte';
  import Disclosure from './Disclosure.svelte';
  import ToolCall from './ToolCall.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { openAgentTab } from '../stores/right-pane.svelte';

  interface Props { snapshot: ExecutionSteps; sessionId: string; filePreviewScope?: FilePreviewScope; }
  let { snapshot, sessionId, filePreviewScope }: Props = $props();
  const turn = $derived(turnStoreState.reducer.turns.find((turn) => turn.turnId === snapshot.turnId && turn.sessionId === sessionId));
  const completed = $derived(snapshot.steps.filter((step) => step.status === 'completed').length);
  const live = $derived(snapshot.steps.some((step) => step.status === 'running' || step.status === 'pending'));
  let expanded = $state(untrack(() => snapshot.steps.some((step) => step.status === 'running' || step.status === 'pending' || step.status === 'blocked')));
  let manual = false;
  let wasLive = $state(untrack(() => live));
  let selected = $state<Record<string, boolean>>({});
  let loaded = $state<Record<string, CanonicalTurnItem>>({});
  let loading = $state('');
  let error = $state('');
  $effect(() => { if (!manual && wasLive && !live) expanded = false; wasLive = live; });
  const contentId = $props.id();
  const overallStatus = $derived(snapshot.steps.some((step) => step.status === 'failed') ? 'error' : snapshot.steps.some((step) => step.status === 'blocked') ? 'pending' : live ? 'running' : snapshot.steps.some((step) => step.status === 'canceled') ? 'cancelled' : 'success');
  const label = (status: ExecutionStepStatus) => i18n.t(`executionSteps.status.${status}`);
  function icon(status: ExecutionStepStatus): IconName {
    return status === 'completed' ? 'check' : status === 'running' ? 'loader' : status === 'blocked' || status === 'failed' ? 'alert-triangle' : status === 'canceled' ? 'close' : 'circle';
  }
  function evidence(stepId: string): CanonicalTurnItem[] {
    const merged = new Map((turn?.items || []).map((item) => [item.itemId, item]));
    for (const item of Object.values(loaded)) if (!merged.has(item.itemId)) merged.set(item.itemId, item);
    return executionStepEvidence(snapshot, stepId, [...merged.values()]);
  }
  async function toggleStep(stepId: string) {
    selected[stepId] = !selected[stepId];
    if (!selected[stepId]) return;
    error = '';
    const step = snapshot.steps.find((step) => step.stepId === stepId);
    if (!step) return;
    const known = new Set(evidence(stepId).flatMap((item) => [item.itemId, item.tool?.callId || '']));
    loading = stepId;
    try {
      for (const itemId of step.evidenceRefs.filter((id) => !known.has(id))) {
        const item = await getAgentMessageItem({ sessionId, turnId: snapshot.turnId, itemId });
        if (item.sessionId === sessionId && item.turnId === snapshot.turnId) loaded[item.itemId] = item;
      }
    } catch { error = i18n.t('executionSteps.loadFailed'); }
    finally { loading = ''; }
  }
</script>

<section class="message-card execution-steps" data-testid="execution-steps" aria-label={i18n.t('executionSteps.title')}>
  <button class="execution-steps-header" type="button" aria-expanded={expanded} aria-controls={contentId} onclick={() => { manual = true; expanded = !expanded; }}>
    <span class="execution-steps-title"><Icon name="list" size={14} /> {i18n.t('executionSteps.title')}</span>
    <ToolCardMeta status={overallStatus} label={i18n.t(`terminalSession.status.${overallStatus}`)} {expanded}><span class="execution-steps-count">{completed} / {snapshot.steps.length}</span></ToolCardMeta>
  </button>
  <Disclosure {expanded}>
    <ol id={contentId}>
      {#each snapshot.steps as step (step.stepId)}
        <li class:running={step.status === 'running'} class:completed={step.status === 'completed'} class:problem={step.status === 'blocked' || step.status === 'failed'}>
          <button class="step-button" type="button" aria-expanded={!!selected[step.stepId]} onclick={() => toggleStep(step.stepId)}>
            <span class="step-icon" aria-label={label(step.status)}><Icon name={icon(step.status)} size={15} class={step.status === 'running' ? 'spinning' : ''} /></span>
            <span class="step-copy"><span class="step-title">{step.title}</span>{#if step.reason}<span class="step-reason">{step.reason}</span>{/if}</span>
            <ToolCardMeta status={step.status === 'completed' ? 'success' : step.status === 'failed' ? 'error' : step.status === 'canceled' ? 'cancelled' : step.status === 'blocked' ? 'pending' : step.status} label={label(step.status)} expanded={!!selected[step.stepId]} />
          </button>
          <Disclosure expanded={!!selected[step.stepId]}>
            <div class="step-details">
              {#if step.dependsOn.length}<div>{i18n.t('executionSteps.dependencies', { titles: step.dependsOn.map((id) => snapshot.steps.find((s) => s.stepId === id)?.title || id).join('、') })}</div>{/if}
              {#each step.taskIds as taskId (taskId)}<button type="button" class="agent-link" onclick={() => openAgentTab(sessionId, taskId)}>{i18n.t('executionSteps.agent')}</button>{/each}
              {#each evidence(step.stepId) as item (item.itemId)}
                {#if item.tool}<ToolCall name={item.tool.name} id={item.tool.callId} input={valueToDisplayText(item.tool.arguments)} output={valueToDisplayText(item.tool.result)} error={item.tool.error} status={item.status === 'completed' ? 'success' : item.status === 'running' ? 'running' : item.status === 'pending' ? 'pending' : item.status === 'cancelled' ? 'cancelled' : item.status === 'indeterminate' ? 'unconfirmed' : 'error'} {filePreviewScope} />
                {:else if item.content}<MarkdownContent content={item.content} {filePreviewScope} />{/if}
              {/each}
              {#if loading === step.stepId}<span>{i18n.t('messageList.loadingOlder')}</span>{:else if error}<span>{error}</span>{:else if evidence(step.stepId).length === 0 && step.taskIds.length === 0}<span>{i18n.t('executionSteps.noEvidence')}</span>{/if}
            </div>
          </Disclosure>
        </li>
      {/each}
    </ol>
    {#if snapshot.explanation}<p class="step-reason">{snapshot.explanation}</p>{/if}
  </Disclosure>
</section>
<style>
  .execution-steps { margin: 8px 0 12px; padding: 0; }
  .execution-steps-header { border:0; background:none; width:100%; padding:9px var(--tool-card-padding); min-height:42px; display:flex; align-items:center; justify-content:space-between; gap:10px; color:var(--foreground-muted); font-size:12px; cursor:pointer; text-align:left; }
  .execution-steps-title { display:flex; gap:6px; align-items:center; color:var(--foreground); font-weight:var(--font-semibold); }
  .execution-steps-count { display:flex; align-items:center; gap:8px; font-variant-numeric:tabular-nums; }
  ol { list-style:none; padding:0 var(--tool-card-padding) 14px; margin:6px 0 0; display:flex; flex-direction:column; gap:7px; }
  li { color:var(--foreground-muted); font-size:13px; line-height:1.45; }
  li.running { color:var(--foreground); } li.problem { color:var(--error); }
  .step-button { display:flex; width:100%; gap:8px; align-items:center; border:0; background:none; padding:0; color:inherit; font:inherit; text-align:left; cursor:pointer; }
  .step-icon { width:16px; display:inline-flex; justify-content:center; padding-top:2px; flex-shrink:0; } li.running .step-icon { color:var(--primary); } li.completed .step-icon { color:var(--success); }
  .step-copy { display:flex; flex-direction:column; min-width:0; flex:1; } .step-title { overflow-wrap:anywhere; } .step-reason { color:var(--foreground-muted); font-size: var(--text-xs); white-space:pre-wrap; }
  .step-details { margin:8px 0 5px 24px; display:flex; flex-direction:column; gap:6px; color:var(--foreground-muted); font-size:12px; }
  .agent-link { background:none; border:0; color:var(--primary); padding:0; cursor:pointer; text-align:left; }
  button:focus-visible { outline:2px solid var(--primary); outline-offset:3px; border-radius:3px; }

</style>

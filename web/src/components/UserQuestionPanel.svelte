<script lang="ts">
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { resolvePendingUserQuestion } from '../stores/user-question-store.svelte';
  import type { PendingUserQuestionDto } from '../shared/rust-backend-types';
  import {
    areUserQuestionsAnswered,
    buildUserQuestionAnswers,
    emptyUserQuestionDrafts,
    isUserQuestionAnswered,
    setUserQuestionOtherText,
    toggleUserQuestionOption,
    toggleUserQuestionOther,
    type UserQuestionDraft,
  } from '../shared/user-question';

  interface Props {
    pending: PendingUserQuestionDto;
    /** 除当前这个之外还在排队等回答的问题数。 */
    remaining?: number;
    /** 在输入框上方叠层里的层级（紧挨输入框为 1）。 */
    level?: number;
  }

  let { pending, remaining = 0, level = 1 }: Props = $props();

  // 同一个问题只在挂载时初始化一次草稿；新问题由上层按 questionId 重建本组件。
  // svelte-ignore state_referenced_locally
  let drafts = $state<UserQuestionDraft[]>(emptyUserQuestionDrafts(pending.questions));
  let activeIndex = $state(0);
  let submitting = $state(false);
  let errorMessage = $state('');
  let otherInputs: Array<HTMLInputElement | undefined> = [];

  const questions = $derived(pending.questions);
  const active = $derived(questions[activeIndex]);
  const draft = $derived(drafts[activeIndex]);
  const allAnswered = $derived(areUserQuestionsAnswered(drafts));
  const isLast = $derived(activeIndex >= questions.length - 1);

  function update(index: number, next: UserQuestionDraft): void {
    drafts = drafts.map((item, itemIndex) => (itemIndex === index ? next : item));
    errorMessage = '';
  }

  function pickOption(label: string): void {
    update(activeIndex, toggleUserQuestionOption(active, draft, label));
    // 单选选完直接进入下一题：减少一次点击，最后一题停下来等用户确认提交。
    if (!active.multiSelect && !isLast) {
      activeIndex += 1;
    }
  }

  function pickOther(): void {
    const index = activeIndex;
    update(index, toggleUserQuestionOther(active, draft));
    if (drafts[index].otherActive) {
      queueMicrotask(() => otherInputs[index]?.focus());
    }
  }

  async function submit(): Promise<void> {
    if (submitting || !allAnswered) return;
    submitting = true;
    errorMessage = '';
    try {
      await resolvePendingUserQuestion(pending.sessionId, pending.questionId, {
        kind: 'answered',
        answers: buildUserQuestionAnswers(questions, drafts),
      });
    } catch (error) {
      errorMessage = error instanceof Error ? error.message : i18n.t('userQuestion.submitFailed');
    } finally {
      submitting = false;
    }
  }

  async function skip(): Promise<void> {
    if (submitting) return;
    submitting = true;
    errorMessage = '';
    try {
      await resolvePendingUserQuestion(pending.sessionId, pending.questionId, { kind: 'skipped' });
    } catch (error) {
      errorMessage = error instanceof Error ? error.message : i18n.t('userQuestion.submitFailed');
    } finally {
      submitting = false;
    }
  }

  function advance(): void {
    if (!isLast) activeIndex += 1;
    else void submit();
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.defaultPrevented || event.isComposing) return;
    const target = event.target as HTMLElement | null;
    const typing = target?.tagName === 'INPUT' || target?.tagName === 'TEXTAREA';
    if (event.key === 'Enter' && !event.shiftKey) {
      if (typing && !isUserQuestionAnswered(draft)) return;
      if (isUserQuestionAnswered(draft)) {
        event.preventDefault();
        advance();
      }
      return;
    }
    if (typing || event.metaKey || event.ctrlKey || event.altKey) return;
    // 数字键直接选第 N 个选项；“其他”排在预设选项之后。
    const number = Number(event.key);
    if (Number.isInteger(number) && number >= 1) {
      if (number <= active.options.length) {
        event.preventDefault();
        pickOption(active.options[number - 1].label);
      } else if (number === active.options.length + 1) {
        event.preventDefault();
        pickOther();
      }
    }
  }
</script>

<!-- 模型向用户提出的选择题：贴在输入框上沿，回答后这一轮继续。键盘快捷键只在焦点位于面板内时生效。 -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
  class="user-question dock-card"
  style="--dock-level: {level}"
  role="group"
  aria-label={i18n.t('userQuestion.title')}
  data-user-question-id={pending.questionId}
  onkeydown={handleKeydown}
>
  <header class="uq-head dock-header">
    <span class="dock-lead"><Icon name="question" size={14} /></span>
    <span class="dock-title">{i18n.t('userQuestion.title')}</span>
    {#if questions.length > 1}
      <div class="uq-steps" role="tablist">
        {#each questions as item, index (index)}
          <button
            type="button"
            role="tab"
            class="uq-step"
            class:active={index === activeIndex}
            class:done={isUserQuestionAnswered(drafts[index])}
            aria-selected={index === activeIndex}
            onclick={() => (activeIndex = index)}
          >
            {#if isUserQuestionAnswered(drafts[index])}<Icon name="check" size={10} />{/if}
            {item.header}
          </button>
        {/each}
      </div>
      <span class="uq-progress dock-meta">{activeIndex + 1} / {questions.length}</span>
    {:else}
      <span class="uq-badge">{active.header}</span>
    {/if}
    {#if remaining > 0}
      <span class="uq-queued dock-meta">{i18n.t('userQuestion.moreWaiting', { count: remaining })}</span>
    {/if}
  </header>

  <div class="uq-ask">
    <p class="uq-question">{active.question}</p>
    {#if active.multiSelect && !/多选|multiple|all that apply/i.test(active.question)}
      <span class="uq-tag">{i18n.t('userQuestion.multiHint')}</span>
    {/if}
  </div>

  <div class="uq-options" role={active.multiSelect ? 'group' : 'radiogroup'}>
    {#each active.options as option, index (option.label)}
      {@const checked = draft.selected.includes(option.label)}
      <button
        type="button"
        class="uq-option"
        class:checked
        role={active.multiSelect ? 'checkbox' : 'radio'}
        aria-checked={checked}
        disabled={submitting}
        onclick={() => pickOption(option.label)}
      >
        <span class="uq-mark" class:square={active.multiSelect} aria-hidden="true">
          {#if checked}<Icon name="check" size={10} />{/if}
        </span>
        <span class="uq-text">
          <span class="uq-label">{option.label}</span>
          {#if option.description}<span class="uq-desc">{option.description}</span>{/if}
        </span>
        <kbd class="uq-key" aria-hidden="true">{index + 1}</kbd>
      </button>
    {/each}

    <div class="uq-option uq-other" class:checked={draft.otherActive}>
      <button
        type="button"
        class="uq-other-toggle"
        role={active.multiSelect ? 'checkbox' : 'radio'}
        aria-checked={draft.otherActive}
        disabled={submitting}
        onclick={pickOther}
      >
        <span class="uq-mark" class:square={active.multiSelect} aria-hidden="true">
          {#if draft.otherActive}<Icon name="check" size={10} />{/if}
        </span>
        <span class="uq-text"><span class="uq-label">{i18n.t('userQuestion.other')}</span></span>
        <kbd class="uq-key" aria-hidden="true">{active.options.length + 1}</kbd>
      </button>
      {#if draft.otherActive}
        <input
          class="uq-other-input"
          type="text"
          bind:this={otherInputs[activeIndex]}
          value={draft.other}
          placeholder={i18n.t('userQuestion.otherPlaceholder')}
          disabled={submitting}
          oninput={(event) => update(activeIndex, setUserQuestionOtherText(draft, event.currentTarget.value))}
        />
      {/if}
    </div>
  </div>

  {#if errorMessage}
    <div class="uq-error" role="alert">{errorMessage}</div>
  {/if}

  <footer class="uq-foot">
    <span class="uq-keyhint dock-meta">{i18n.t('userQuestion.keyHint')}</span>
    <span class="uq-spacer"></span>
    <button type="button" class="uq-button dock-btn" disabled={submitting} onclick={() => void skip()}>
      {i18n.t('userQuestion.skip')}
    </button>
    {#if !isLast}
      <button
        type="button"
        class="uq-button dock-btn dock-btn--primary"
        disabled={submitting || !isUserQuestionAnswered(draft)}
        onclick={advance}
      >
        {i18n.t('userQuestion.next')}
      </button>
    {:else}
      <button
        type="button"
        class="uq-button dock-btn dock-btn--primary"
        disabled={submitting || !allAnswered}
        onclick={() => void submit()}
      >
        {submitting ? i18n.t('userQuestion.submitting') : i18n.t('userQuestion.submit')}
      </button>
    {/if}
  </footer>
</section>

<style>
  /* 叠层的外形（上圆角、无下边线、缩进台阶、背景退后）由全局 .dock-card 统一提供。 */
  .user-question {
    z-index: 1;
    display: flex;
    flex-direction: column;
    /* 窗口较矮、又有其他面板叠在上面时，不能把消息区挤没：限高并在卡片内部滚动。 */
    max-height: min(46vh, 420px);
    overflow-y: auto;
    flex: 0 0 auto;
    animation: uq-rise 160ms ease-out;
  }

  @keyframes uq-rise {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .user-question {
      animation: none;
    }
  }

  .uq-head {
    flex-wrap: wrap;
  }

  .uq-badge {
    display: inline-flex;
    align-items: center;
    padding: 1px var(--space-2);
    border-radius: 999px;
    background: var(--primary-muted);
    color: var(--primary);
    font-size: var(--text-xs);
    font-weight: 600;
    line-height: 1.5;
  }

  .uq-steps {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--foreground) 6%, transparent);
    min-width: 0;
    overflow-x: auto;
  }

  .uq-step {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px var(--space-3);
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    white-space: nowrap;
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .uq-step:hover {
    color: var(--foreground);
  }

  .uq-step.active {
    background: var(--vscode-input-background);
    color: var(--primary);
    font-weight: 600;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.25);
  }

  .uq-step.done:not(.active) {
    color: var(--foreground);
  }

  .uq-queued {
    margin-left: auto;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .uq-progress {
    margin-left: auto;
    font-variant-numeric: tabular-nums;
  }

  .uq-ask {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
  }

  .uq-question {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: 600;
    line-height: 1.45;
    overflow-wrap: anywhere;
  }

  .uq-tag {
    flex: 0 0 auto;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--foreground-muted);
    font-size: 10px;
    line-height: 1.7;
  }

  .uq-options {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .uq-option {
    display: flex;
    align-items: flex-start;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    padding: 4px var(--space-2);
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground);
    text-align: left;
    /* 全局把 button 设为 nowrap；选项文字（含长描述）要在卡片内换行，不能整行冲出去。 */
    white-space: normal;
    cursor: pointer;
    font: inherit;
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  button.uq-option:hover:not(:disabled),
  .uq-other:not(.checked):hover {
    background: color-mix(in srgb, var(--foreground) 5%, transparent);
  }

  .uq-option.checked {
    border-color: color-mix(in srgb, var(--primary) 45%, transparent);
    background: var(--primary-muted);
  }

  .uq-option:focus-visible,
  .uq-other-toggle:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--primary) 60%, transparent);
    outline-offset: -2px;
  }

  .uq-option:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .uq-other {
    flex-direction: column;
    align-items: stretch;
    gap: 2px;
    cursor: default;
  }

  .uq-other-toggle {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    padding: 0;
    border: 0;
    background: transparent;
    color: inherit;
    text-align: left;
    white-space: normal;
    cursor: pointer;
    font: inherit;
  }

  .uq-mark {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    border: 1.5px solid color-mix(in srgb, var(--foreground-muted) 70%, transparent);
    border-radius: 50%;
    color: var(--primary-foreground, #fff);
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  .uq-mark.square {
    border-radius: 5px;
  }

  .uq-option.checked .uq-mark {
    border-color: var(--primary);
    background: var(--primary);
  }

  .uq-text {
    display: flex;
    flex: 1 1 auto;
    flex-flow: row wrap;
    align-items: baseline;
    column-gap: var(--space-2);
    row-gap: 0;
    min-width: 0;
  }

  .uq-label {
    font-size: var(--text-sm);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .uq-option.checked .uq-label {
    font-weight: 600;
  }

  .uq-desc {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .uq-key {
    flex: 0 0 auto;
    min-width: 18px;
    padding: 0 4px;
    border: 1px solid color-mix(in srgb, var(--border) 80%, transparent);
    border-radius: 4px;
    color: var(--foreground-muted);
    font: inherit;
    font-size: 10px;
    line-height: 15px;
    text-align: center;
    opacity: 0.7;
  }

  .uq-other-input {
    align-self: flex-end;
    width: calc(100% - 14px - var(--space-2));
    padding: var(--space-1) 0;
    border: 0;
    border-bottom: 1px solid color-mix(in srgb, var(--primary) 55%, transparent);
    background: transparent;
    color: var(--foreground);
    font: inherit;
    font-size: var(--text-sm);
    outline: none;
  }

  .uq-other-input::placeholder {
    color: var(--foreground-muted);
  }

  .uq-error {
    color: var(--error);
    font-size: var(--text-xs);
  }

  .uq-foot {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .uq-spacer {
    flex: 1;
  }





</style>

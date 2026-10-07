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
  }

  let { pending }: Props = $props();

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
  class="user-question"
  role="group"
  aria-label={i18n.t('userQuestion.title')}
  data-user-question-id={pending.questionId}
  onkeydown={handleKeydown}
>
  <header class="uq-head">
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
      <span class="uq-progress">{activeIndex + 1} / {questions.length}</span>
    {:else}
      <span class="uq-badge"><Icon name="question" size={12} />{active.header}</span>
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
    <span class="uq-keyhint">{i18n.t('userQuestion.keyHint')}</span>
    <span class="uq-spacer"></span>
    <button type="button" class="uq-button" disabled={submitting} onclick={() => void skip()}>
      {i18n.t('userQuestion.skip')}
    </button>
    {#if !isLast}
      <button
        type="button"
        class="uq-button uq-button--primary"
        disabled={submitting || !isUserQuestionAnswered(draft)}
        onclick={advance}
      >
        {i18n.t('userQuestion.next')}
      </button>
    {:else}
      <button
        type="button"
        class="uq-button uq-button--primary"
        disabled={submitting || !allAnswered}
        onclick={() => void submit()}
      >
        {submitting ? i18n.t('userQuestion.submitting') : i18n.t('userQuestion.submit')}
      </button>
    {/if}
  </footer>
</section>

<style>
  /* 贴在输入框上沿的卡片：比输入框窄一圈，只有上圆角，下沿盖住输入框的上边框，视觉上与输入框相连。 */
  .user-question {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    width: calc(100% - var(--space-6, 24px) * 2);
    margin: 0 auto -1px;
    padding: var(--space-3) var(--space-3) var(--space-3);
    border: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
    border-bottom: 0;
    border-radius: var(--radius-xl) var(--radius-xl) 0 0;
    background: var(--vscode-input-background);
    box-shadow: 0 -6px 18px -12px rgba(0, 0, 0, 0.35);
    max-height: min(52vh, 420px);
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
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-inline: var(--space-1);
  }

  .uq-badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px var(--space-2);
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

  .uq-progress {
    margin-left: auto;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .uq-ask {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    padding-inline: var(--space-1);
  }

  .uq-question {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-base);
    font-weight: 600;
    line-height: 1.5;
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
    gap: var(--space-3);
    width: 100%;
    padding: var(--space-2) var(--space-3);
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground);
    text-align: left;
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
    gap: var(--space-2);
    cursor: default;
  }

  .uq-other-toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    width: 100%;
    padding: 0;
    border: 0;
    background: transparent;
    color: inherit;
    text-align: left;
    cursor: pointer;
    font: inherit;
  }

  .uq-mark {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    margin-top: 2px;
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
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .uq-label {
    font-size: var(--text-sm);
    line-height: 1.45;
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
    width: calc(100% - 16px - var(--space-3));
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
    padding-inline: var(--space-1);
  }

  .uq-foot {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-inline: var(--space-1);
  }

  .uq-keyhint {
    color: var(--foreground-muted);
    font-size: 11px;
    opacity: 0.85;
  }

  .uq-spacer {
    flex: 1;
  }

  .uq-button {
    min-height: 28px;
    padding: 0 var(--space-3);
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .uq-button:hover:not(:disabled) {
    background: color-mix(in srgb, var(--foreground) 7%, transparent);
    color: var(--foreground);
  }

  .uq-button--primary {
    padding: 0 var(--space-4);
    background: var(--primary);
    color: var(--primary-foreground, #fff);
    font-weight: 600;
  }

  .uq-button--primary:hover:not(:disabled) {
    background: var(--primary-hover, var(--primary));
    color: var(--primary-foreground, #fff);
  }

  .uq-button:disabled {
    opacity: 0.45;
    cursor: default;
  }
</style>

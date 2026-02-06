<script lang="ts">
  import { onMount } from 'svelte';
  import { initializeState } from './stores/messages.svelte';
  import Header from './components/Header.svelte';
  import TopTabs from './components/TopTabs.svelte';
  import ThreadPanel from './components/ThreadPanel.svelte';
  import TasksPanel from './components/TasksPanel.svelte';
  import EditsPanel from './components/EditsPanel.svelte';
  import KnowledgePanel from './components/KnowledgePanel.svelte';
  import SettingsPanel from './components/SettingsPanel.svelte';
  import SkillPopup from './components/SkillPopup.svelte';
  import ToastContainer from './components/ToastContainer.svelte';
  import MarkdownContent from './components/MarkdownContent.svelte';
  import { vscode } from './lib/vscode-bridge';
  import { getState, setCurrentTopTab, setIsProcessing } from './stores/messages.svelte';

  type TopTabType = 'thread' | 'tasks' | 'edits' | 'knowledge';

  // 当前激活的顶部 Tab
  const appState = getState();

  // 安全获取顶部 Tab（映射非顶部 Tab 到默认值）
  const currentTopTab = $derived<TopTabType>(
    ['thread', 'tasks', 'edits', 'knowledge'].includes(appState.currentTopTab as string)
      ? (appState.currentTopTab as TopTabType)
      : 'thread'
  );

  // 设置面板是否打开
  let settingsOpen = $state(false);

  // 技能弹窗是否打开
  let skillPopupOpen = $state(false);

  // 交互输入
  let questionAnswer = $state('');
  let clarificationAnswer = $state('');
  let workerQuestionAnswer = $state('');

  const pendingConfirmation = $derived(appState.pendingConfirmation);
  const pendingRecovery = $derived(appState.pendingRecovery);
  const pendingQuestion = $derived(appState.pendingQuestion);
  const pendingClarification = $derived(appState.pendingClarification);
  const pendingWorkerQuestion = $derived(appState.pendingWorkerQuestion);
  const pendingToolAuthorization = $derived(appState.pendingToolAuthorization);
  const interactionMode = $derived(appState.appState?.interactionMode || 'auto');

  function handleTabChange(tab: TopTabType) {
    setCurrentTopTab(tab);
  }

  function openSettings() {
    settingsOpen = true;
  }

  function closeSettings() {
    settingsOpen = false;
  }

  function closeSkillPopup() {
    skillPopupOpen = false;
  }

  function confirmPlan(confirmed: boolean) {
    vscode.postMessage({ type: 'confirmPlan', confirmed });
    appState.pendingConfirmation = null;
    if (confirmed) setIsProcessing(true);
  }

  function confirmRecovery(decision: 'retry' | 'rollback' | 'continue') {
    vscode.postMessage({ type: 'confirmRecovery', decision });
    appState.pendingRecovery = null;
    setIsProcessing(true);
  }

  function submitQuestion(cancelled = false) {
    const answer = cancelled ? null : (questionAnswer.trim() || null);
    vscode.postMessage({ type: 'answerQuestions', answer });
    appState.pendingQuestion = null;
    questionAnswer = '';
    if (!cancelled) setIsProcessing(true);
  }

  function submitClarification(cancelled = false) {
    const answer = cancelled ? null : (clarificationAnswer.trim() || '');
    vscode.postMessage({
      type: 'answerClarification',
      answers: cancelled ? null : { _userResponse: answer },
      additionalInfo: answer,
    });
    appState.pendingClarification = null;
    clarificationAnswer = '';
    if (!cancelled) setIsProcessing(true);
  }

  function submitWorkerQuestion(cancelled = false) {
    const answer = cancelled ? null : (workerQuestionAnswer.trim() || '');
    vscode.postMessage({ type: 'answerWorkerQuestion', answer });
    appState.pendingWorkerQuestion = null;
    workerQuestionAnswer = '';
    if (!cancelled) setIsProcessing(true);
  }

  function respondToolAuthorization(allowed: boolean) {
    const requestId = appState.pendingToolAuthorization?.requestId;
    if (!requestId) {
      appState.pendingToolAuthorization = null;
      return;
    }
    vscode.postMessage({ type: 'toolAuthorizationResponse', requestId, allowed });
    appState.pendingToolAuthorization = null;
    if (allowed) setIsProcessing(true);
  }

  // 初始化状态
  onMount(() => {
    initializeState();
    console.log('[App] Svelte webview 已初始化');

    // 监听从 InputArea 发来的自定义事件
    const customEventHandler = () => {
      skillPopupOpen = true;
    };
    window.addEventListener('openSkillPopup', customEventHandler);

    return () => {
      window.removeEventListener('openSkillPopup', customEventHandler);
    };
  });
</script>

<div class="app-container">
  <!-- 顶部标题栏 -->
  <Header onOpenSettings={openSettings} />

  <!-- 顶部 Tab 栏：对话/任务/变更/知识 -->
  <TopTabs activeTopTab={currentTopTab} onTabChange={handleTabChange} />

  <!-- Tab 内容区域 -->
  <div class="tab-content-wrapper">
    {#if currentTopTab === 'thread'}
      <ThreadPanel />
    {:else if currentTopTab === 'tasks'}
      <TasksPanel />
    {:else if currentTopTab === 'edits'}
      <EditsPanel />
    {:else if currentTopTab === 'knowledge'}
      <KnowledgePanel />
    {/if}
  </div>

  <!-- 设置面板（覆盖层） -->
  {#if settingsOpen}
    <SettingsPanel onClose={closeSettings} />
  {/if}

  <!-- 技能弹窗 -->
  <SkillPopup visible={skillPopupOpen} onClose={closeSkillPopup} />

  {#if pendingConfirmation && interactionMode === 'ask'}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog plan-confirm-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>执行计划确认</h3>
        </div>
        <div class="modal-body">
          {#if pendingConfirmation.formattedPlan}
            <MarkdownContent content={pendingConfirmation.formattedPlan} />
          {:else}
            <p>需要确认执行计划，是否继续？</p>
          {/if}
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => confirmPlan(false)}>取消</button>
          <button class="modal-btn primary" onclick={() => confirmPlan(true)}>确认执行</button>
        </div>
      </div>
    </div>
  {/if}

  {#if pendingRecovery && interactionMode === 'ask'}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>恢复策略确认</h3>
        </div>
        <div class="modal-body">
          <p>任务执行失败，需要选择恢复策略：</p>
          {#if pendingRecovery.error}
            <pre class="modal-pre">{String(pendingRecovery.error)}</pre>
          {/if}
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => confirmRecovery('continue')}>继续</button>
          <button class="modal-btn secondary" disabled={!pendingRecovery.canRollback} onclick={() => confirmRecovery('rollback')}>回滚</button>
          <button class="modal-btn primary" disabled={!pendingRecovery.canRetry} onclick={() => confirmRecovery('retry')}>重试</button>
        </div>
      </div>
    </div>
  {/if}

  {#if pendingQuestion && interactionMode === 'ask'}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>需要补充信息</h3>
        </div>
        <div class="modal-body">
          <ol class="question-list">
            {#each pendingQuestion.questions as q}
              <li>{q}</li>
            {/each}
          </ol>
          <textarea class="modal-textarea" bind:value={questionAnswer} placeholder="请输入补充信息..."></textarea>
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => submitQuestion(true)}>取消</button>
          <button class="modal-btn primary" onclick={() => submitQuestion(false)}>提交</button>
        </div>
      </div>
    </div>
  {/if}

  {#if pendingClarification && interactionMode === 'ask'}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>澄清问题</h3>
        </div>
        <div class="modal-body">
          {#if pendingClarification.context}
            <div class="modal-context">{pendingClarification.context}</div>
          {/if}
          <ol class="question-list">
            {#each pendingClarification.questions as q}
              <li>{q}</li>
            {/each}
          </ol>
          <textarea class="modal-textarea" bind:value={clarificationAnswer} placeholder="请输入补充信息..."></textarea>
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => submitClarification(true)}>取消</button>
          <button class="modal-btn primary" onclick={() => submitClarification(false)}>提交</button>
        </div>
      </div>
    </div>
  {/if}

  {#if pendingWorkerQuestion && interactionMode === 'ask'}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>{pendingWorkerQuestion.workerId} 提问</h3>
        </div>
        <div class="modal-body">
          <p>{pendingWorkerQuestion.question}</p>
          <textarea class="modal-textarea" bind:value={workerQuestionAnswer} placeholder="请输入回答..."></textarea>
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => submitWorkerQuestion(true)}>取消</button>
          <button class="modal-btn primary" onclick={() => submitWorkerQuestion(false)}>提交</button>
        </div>
      </div>
    </div>
  {/if}

  {#if pendingToolAuthorization && interactionMode === 'ask'}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>工具授权请求</h3>
        </div>
        <div class="modal-body">
          <p>工具: <strong>{pendingToolAuthorization.toolName}</strong></p>
          <pre class="modal-pre">{JSON.stringify(pendingToolAuthorization.toolArgs, null, 2)}</pre>
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => respondToolAuthorization(false)}>拒绝</button>
          <button class="modal-btn primary" onclick={() => respondToolAuthorization(true)}>允许</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- Toast 通知容器 -->
  <ToastContainer />
</div>

<style>
  .app-container {
    display: flex;
    flex-direction: column;
    height: 100%;
    width: 100%;
    overflow: hidden;
    background: var(--background);
  }

  .tab-content-wrapper {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .modal-overlay {
    position: fixed;
    inset: 0;
    background: var(--overlay);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-modal);
  }

  .modal-dialog {
    width: 520px;
    max-width: 92vw;
    max-height: 80vh;
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    overflow: hidden;
    display: flex;
    flex-direction: column;
    box-shadow: var(--shadow-xl);
  }

  .plan-confirm-dialog {
    width: 720px;
  }

  .modal-header {
    padding: var(--space-4);
    border-bottom: 1px solid var(--border);
  }

  .modal-body {
    padding: var(--space-4);
    overflow-y: auto;
  }

  .modal-footer {
    padding: var(--space-3) var(--space-4);
    border-top: 1px solid var(--border);
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .modal-btn {
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface-1);
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    transition: all var(--transition-fast);
  }

  .modal-btn:hover {
    background: var(--surface-hover);
  }

  .modal-btn.primary {
    background: var(--primary);
    border-color: var(--primary);
    color: white;
  }

  .modal-btn.primary:hover {
    opacity: 0.9;
  }

  .modal-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .modal-textarea {
    width: 100%;
    min-height: 120px;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
    resize: vertical;
  }

  .modal-pre {
    white-space: pre-wrap;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .question-list {
    margin: 0 0 var(--space-3);
    padding-left: var(--space-4);
    color: var(--foreground);
  }

  .modal-context {
    margin-bottom: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }
</style>

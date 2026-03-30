<script lang="ts">
  import Header from './components/Header.svelte';
  import TopTabs from './components/TopTabs.svelte';
  import ThreadPanel from './components/ThreadPanel.svelte';
  import TasksPanel from './components/TasksPanel.svelte';
  import EditsPanel from './components/EditsPanel.svelte';
  import KnowledgePanel from './components/KnowledgePanel.svelte';
  import SettingsPanel from './components/SettingsPanel.svelte';
  import ToastContainer from './components/ToastContainer.svelte';
  import Icon from './components/Icon.svelte';
  import { vscode } from './lib/vscode-bridge';
  import { getState, setCurrentTopTab, setIsProcessing, messagesState } from './stores/messages.svelte';
  import { i18n } from './stores/i18n.svelte';

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

  // 交互输入
  let clarificationAnswer = $state('');
  let workerQuestionAnswer = $state('');

  const pendingRecovery = $derived(appState.pendingRecovery);
  const pendingClarification = $derived(appState.pendingClarification);
  const pendingWorkerQuestion = $derived(appState.pendingWorkerQuestion);

  // 启动连接状态：后端 bootstrap 数据尚未就绪时显示等待提示
  const isBootstrapping = $derived(!messagesState.bootstrapped);

  function handleTabChange(tab: TopTabType) {
    setCurrentTopTab(tab);
  }

  function openSettings() {
    settingsOpen = true;
  }

  function closeSettings() {
    settingsOpen = false;
  }

  function confirmRecovery(decision: 'retry' | 'rollback' | 'continue') {
    vscode.postMessage({ type: 'confirmRecovery', decision });
    appState.pendingRecovery = null;
    setIsProcessing(true);
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

</script>

<div class="app-container">
  <!-- 顶部标题栏 -->
  <Header onOpenSettings={openSettings} />

  <!-- 顶部 Tab 栏：对话/任务/变更/知识 -->
  <TopTabs activeTopTab={currentTopTab} onTabChange={handleTabChange} />

  <!-- Tab 内容区域：主对话面板常驻以保留输入草稿，其余非主线面板仅在激活时挂载 -->
  <div class="tab-content-wrapper">
    {#if isBootstrapping}
      <!-- 启动连接等待层：后端 bootstrap 数据尚未就绪 -->
      <div class="bootstrap-overlay">
        <div class="bootstrap-content">
          <div class="bootstrap-spinner">
            <Icon name="loader" size={32} />
          </div>
          <p class="bootstrap-title">{i18n.t('app.bootstrapConnecting')}</p>
          <p class="bootstrap-hint">{i18n.t('app.bootstrapConnectingHint')}</p>
        </div>
      </div>
    {/if}
    <div class="top-tab-pane" class:active={currentTopTab === 'thread'}>
      <ThreadPanel isTopActive={currentTopTab === 'thread'} />
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'tasks'}>
      {#if currentTopTab === 'tasks'}
        <TasksPanel />
      {/if}
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'edits'}>
      {#if currentTopTab === 'edits'}
        <EditsPanel />
      {/if}
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'knowledge'}>
      {#if currentTopTab === 'knowledge'}
        <KnowledgePanel />
      {/if}
    </div>
  </div>

  <!-- 设置面板（覆盖层） -->
  {#if settingsOpen}
    <SettingsPanel onClose={closeSettings} />
  {/if}

  {#if pendingRecovery}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>{i18n.t('app.recoveryTitle')}</h3>
        </div>
        <div class="modal-body">
          <p>{i18n.t('app.recoveryMessage')}</p>
          {#if pendingRecovery.error}
            <pre class="modal-pre">{String(pendingRecovery.error)}</pre>
          {/if}
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => confirmRecovery('continue')}>{i18n.t('app.recoveryContinue')}</button>
          <button class="modal-btn secondary" disabled={!pendingRecovery.canRollback} onclick={() => confirmRecovery('rollback')}>{i18n.t('app.recoveryRollback')}</button>
          <button class="modal-btn primary" disabled={!pendingRecovery.canRetry} onclick={() => confirmRecovery('retry')}>{i18n.t('app.recoveryRetry')}</button>
        </div>
      </div>
    </div>
  {/if}

  {#if pendingClarification}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>{i18n.t('app.clarificationTitle')}</h3>
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
          <textarea class="modal-textarea" bind:value={clarificationAnswer} placeholder={i18n.t('app.clarificationPlaceholder')}></textarea>
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => submitClarification(true)}>{i18n.t('app.clarificationCancel')}</button>
          <button class="modal-btn primary" onclick={() => submitClarification(false)}>{i18n.t('app.clarificationSubmit')}</button>
        </div>
      </div>
    </div>
  {/if}

  {#if pendingWorkerQuestion}
    <div class="modal-overlay" role="presentation">
      <div class="modal-dialog" role="dialog" aria-modal="true" tabindex="-1">
        <div class="modal-header">
          <h3>{i18n.t('app.workerQuestionTitle', { workerId: pendingWorkerQuestion.workerId })}</h3>
        </div>
        <div class="modal-body">
          <p>{pendingWorkerQuestion.question}</p>
          <textarea class="modal-textarea" bind:value={workerQuestionAnswer} placeholder={i18n.t('app.workerQuestionPlaceholder')}></textarea>
        </div>
        <div class="modal-footer">
          <button class="modal-btn secondary" onclick={() => submitWorkerQuestion(true)}>{i18n.t('app.workerQuestionCancel')}</button>
          <button class="modal-btn primary" onclick={() => submitWorkerQuestion(false)}>{i18n.t('app.workerQuestionSubmit')}</button>
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
    min-height: 0; /* flex 布局防溢出：防止子元素撑破容器产生页面级滚动条 */
    overflow: hidden;
    display: flex;
    flex-direction: column;
    position: relative;
  }

  /* 启动连接等待覆盖层 */
  .bootstrap-overlay {
    position: absolute;
    inset: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--background);
    animation: bootstrap-fade-in 0.3s ease-out;
  }

  .bootstrap-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    text-align: center;
    padding: 0 24px;
  }

  .bootstrap-spinner {
    color: var(--foreground-muted, #888);
    animation: bootstrap-spin 1.2s linear infinite;
  }

  .bootstrap-title {
    font-size: 15px;
    font-weight: 500;
    color: var(--foreground, #ccc);
    margin: 0;
  }

  .bootstrap-hint {
    font-size: 12px;
    color: var(--foreground-muted, #888);
    margin: 0;
  }

  @keyframes bootstrap-spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  @keyframes bootstrap-fade-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  /* 顶部 Tab 面板：默认隐藏，激活时显示（与 ThreadPanel 底部 Tab 同一模式） */
  .top-tab-pane {
    display: none;
    flex: 1;
    min-height: 0;
  }

  .top-tab-pane.active {
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

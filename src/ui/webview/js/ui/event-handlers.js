// 事件处理模块
// 此文件包含所有用户交互事件的处理函数和事件绑定初始化

import {
  threadMessages,
  cliOutputs,
  currentSessionId,
  currentTopTab,
  currentBottomTab,
  isProcessing,
  sessions,
  pendingChanges,
  attachedImages,
  setAppState,
  setIsProcessing,
  setCurrentTopTab,
  setCurrentBottomTab,
  setProcessingActor,
  saveWebviewState
} from '../core/state.js';

import {
  escapeHtml,
  formatTimestamp,
  smoothScrollToBottom
} from '../core/utils.js';

import {
  postMessage,
  executeTask,
  interruptTask,
  confirmPlan,
  answerQuestions,
  createNewSession
} from '../core/vscode-api.js';

import {
  renderMainContent,
  scheduleRenderMainContent,
  renderImagePreviews,
  renderTasksView,
  renderEditsView,
  renderSkillLibrary,
  renderRepositoryManagementList,
  renderSkillsToolList,
  showMCPDialog,
  toggleDependencyPanel,
  updateEditsBadge,
  updateTasksBadge
} from './message-renderer.js';

import {
  handleClarificationAnswer,
  handleWorkerQuestionAnswer,
  handleQuestionAnswer,
  handlePlanConfirmation,
  showQuestionRequest,
  showWorkerQuestion,
  showPlanConfirmation,
  updateCliDots,
  updatePromptEnhanceStatus,
  handlePromptEnhanced,
  loadSessionMessages,
  showToast,
  addSystemMessage
} from './message-handler.js';

import { showDependencyAnalysis } from './message-renderer.js';

import {
  initializeSettingsPanel
} from './settings-handler.js';

let currentInteractionMode = 'agent';
let currentWorkerModel = 'claude';
let workerConfigs = {
  claude: null,
  codex: null,
  gemini: null
};

// ============================================
// 辅助函数
// ============================================

function hasPendingClarification() {
  return !!window._pendingClarification;
}

function hasPendingWorkerQuestion() {
  return !!window._pendingWorkerQuestion;
}

function hasPendingQuestion() {
  return threadMessages.some(m =>
    m.type === 'question_request' &&
    m.isPending
  );
}

function hasPendingConfirmation() {
  return threadMessages.some(m =>
    m.type === 'plan_confirmation' &&
    m.isPending
  );
}

function interruptCurrentOperation() {
  postMessage({ type: 'interrupt' });
  showToast('正在中断任务...', 'info');
}

export function updateInteractionModeUI(mode) {
  currentInteractionMode = mode || 'agent';
  const selector = document.getElementById('mode-selector');
  if (selector) selector.value = currentInteractionMode;
}

export function getModeDisplayName(mode) {
  const map = {
    agent: 'Agent',
    ask: 'Ask',
    auto: 'Auto'
  };
  return map[mode] || mode || 'Agent';
}

function savePromptEnhanceConfig() {
  const urlInput = document.getElementById('prompt-enhance-url');
  const keyInput = document.getElementById('prompt-enhance-key');
  const config = {
    baseUrl: urlInput ? urlInput.value : '',
    apiKey: keyInput ? keyInput.value : ''
  };
  postMessage({ type: 'updatePromptEnhance', config });
}

export function showRepositoryManagementDialog() {
  const dialogHTML = `
    <div class="modal-overlay" id="repo-manage-overlay" onclick="if(event.target===this) closeRepositoryManagementDialog()">
      <div class="modal-dialog" style="width: 640px; max-width: 90vw;">
        <div class="modal-header">
          <h3>管理技能仓库</h3>
          <button class="modal-close" onclick="closeRepositoryManagementDialog()">×</button>
        </div>
        <div class="modal-body">
          <div class="repo-add-section">
            <div class="repo-add-form">
              <div class="form-field" style="flex: 1; margin-bottom: 0;">
                <label>仓库 URL</label>
                <input type="text" id="repo-url-input" placeholder="https://example.com/skills.json">
              </div>
              <button class="settings-btn primary" onclick="addRepositoryFromDialog()">
                <svg viewBox="0 0 16 16" fill="currentColor">
                  <path d="M8 2a.5.5 0 0 1 .5.5v5h5a.5.5 0 0 1 0 1h-5v5a.5.5 0 0 1-1 0v-5h-5a.5.5 0 0 1 0-1h5v-5A.5.5 0 0 1 8 2z"/>
                </svg>
                <span>添加</span>
              </button>
            </div>
            <div class="repo-add-hint">仓库名称将自动从 URL 获取</div>
          </div>

          <div class="repo-list-title">已添加的仓库</div>
          <div id="repo-manage-list" class="repo-manage-list"></div>
        </div>
        <div class="modal-footer">
          <button class="settings-btn" onclick="closeRepositoryManagementDialog()">关闭</button>
        </div>
      </div>
    </div>
  `;

  document.body.insertAdjacentHTML('beforeend', dialogHTML);
  renderRepositoryManagementList();
}

export function closeRepositoryManagementDialog() {
  const dialog = document.getElementById('repo-manage-overlay');
  if (dialog) dialog.remove();
}

export function addRepositoryFromDialog() {
  const input = document.getElementById('repo-url-input');
  const url = input ? input.value.trim() : '';
  if (!url) {
    alert('请输入仓库 URL');
    return;
  }
  postMessage({ type: 'addRepository', url });
  if (input) input.value = '';
}

export function refreshRepositoryInDialog(id) {
  const refreshButton = document.getElementById(`refresh-btn-${id}`);
  if (refreshButton) {
    const svg = refreshButton.querySelector('svg');
    if (svg) {
      svg.style.animation = 'spin 1s linear infinite';
    }
    refreshButton.disabled = true;
    refreshButton.style.opacity = '0.6';
    refreshButton.style.cursor = 'not-allowed';
  }

  postMessage({ type: 'refreshRepository', repositoryId: id });

  setTimeout(() => {
    if (refreshButton) {
      const svg = refreshButton.querySelector('svg');
      if (svg) {
        svg.style.animation = '';
      }
      refreshButton.disabled = false;
      refreshButton.style.opacity = '1';
      refreshButton.style.cursor = 'pointer';
    }
  }, 2000);
}

export function deleteRepositoryFromDialog(id) {
  if (confirm('确定要删除此仓库吗？')) {
    postMessage({ type: 'deleteRepository', repositoryId: id });
  }
}

export function showSkillLibraryDialog(skills) {
  const dialogHTML = `
    <div class="modal-overlay" id="skill-library-overlay">
      <div class="modal-dialog" style="width: 640px; max-width: 90vw;">
        <div class="modal-header">
          <h3>Skill 库</h3>
          <button class="modal-close" id="skill-library-close">×</button>
        </div>
        <div class="modal-body">
          <div class="skill-library-search" style="margin-bottom: var(--spacing-4);">
            <input type="text" id="skill-search" placeholder="搜索 Skill..."
              style="width: 100%; height: 36px; padding: var(--spacing-2) var(--spacing-3);
              border: 1px solid var(--vscode-input-border);
              background: var(--vscode-input-background);
              color: var(--vscode-input-foreground);
              border-radius: var(--radius-2);
              font-size: var(--font-size-2);
              outline: none;
              transition: border-color var(--transition-fast);">
          </div>
          <div class="skill-library-list" id="skill-library-list" style="max-height: 480px; overflow-y: auto;"></div>
        </div>
        <div class="modal-footer">
          <button class="settings-btn" id="skill-library-cancel">关闭</button>
        </div>
      </div>
    </div>
  `;

  const oldDialog = document.getElementById('skill-library-overlay');
  if (oldDialog) oldDialog.remove();
  document.body.insertAdjacentHTML('beforeend', dialogHTML);

  document.getElementById('skill-library-close').addEventListener('click', closeSkillLibraryDialog);
  document.getElementById('skill-library-cancel').addEventListener('click', closeSkillLibraryDialog);
  document.getElementById('skill-library-overlay').addEventListener('click', (e) => {
    if (e.target.id === 'skill-library-overlay') closeSkillLibraryDialog();
  });

  const searchInput = document.getElementById('skill-search');
  if (searchInput) {
    searchInput.addEventListener('input', (e) => {
      const query = e.target.value.toLowerCase();
      const items = document.querySelectorAll('.skill-library-item');
      items.forEach(item => {
        const name = item.dataset.skillName?.toLowerCase() || '';
        const desc = item.dataset.skillDesc?.toLowerCase() || '';
        if (name.includes(query) || desc.includes(query)) {
          item.style.display = '';
        } else {
          item.style.display = 'none';
        }
      });
    });

    searchInput.addEventListener('focus', function() {
      this.style.borderColor = 'var(--vscode-focusBorder)';
    });

    searchInput.addEventListener('blur', function() {
      this.style.borderColor = 'var(--vscode-input-border)';
    });
  }

  if (skills) {
    renderSkillLibrary(skills);
  } else {
    postMessage({ type: 'loadSkillLibrary' });
  }
}

export function closeSkillLibraryDialog() {
  const dialog = document.getElementById('skill-library-overlay');
  if (dialog) dialog.remove();
}

export function installSkill(skillFullName) {
  postMessage({ type: 'installSkill', skillId: skillFullName });
  closeSkillLibraryDialog();
}

function displayWorkerConfig(worker) {
  const config = workerConfigs[worker];
  if (!config) return;

  const baseUrlInput = document.getElementById('worker-base-url');
  const apiKeyInput = document.getElementById('worker-api-key');
  const modelInput = document.getElementById('worker-model');
  const providerSelect = document.getElementById('worker-provider');
  const enabledCheckbox = document.getElementById('worker-enabled');

  if (baseUrlInput) baseUrlInput.value = config.baseUrl || '';
  if (apiKeyInput) apiKeyInput.value = config.apiKey || '';
  if (modelInput) modelInput.value = config.model || '';
  if (providerSelect) providerSelect.value = config.provider || 'anthropic';
  if (enabledCheckbox) enabledCheckbox.checked = config.enabled !== false;
}

export function setWorkerConfigs(configs) {
  workerConfigs = configs || { claude: null, codex: null, gemini: null };
  displayWorkerConfig(currentWorkerModel);
}

function initWorkerModelConfig() {
  postMessage({ type: 'loadAllWorkerConfigs' });
}

function initOrchestratorConfig() {
  postMessage({ type: 'loadOrchestratorConfig' });
}

function initCompressorConfig() {
  postMessage({ type: 'loadCompressorConfig' });
}

// ============================================
// Tab 切换
// ============================================

export function handleTopTabClick(tabName) {
  setCurrentTopTab(tabName);

  // 更新 Tab 按钮状态
  document.querySelectorAll('.top-tab').forEach(tab => {
    tab.classList.toggle('active', tab.dataset.tab === tabName);
  });

  // 显示对应的 tab-panel
  document.querySelectorAll('.tab-panel').forEach(panel => {
    panel.classList.toggle('active', panel.id === `panel-${tabName}`);
  });

  // 根据切换的 Tab 渲染对应内容
  if (tabName === 'thread') {
    renderMainContent();
  } else if (tabName === 'tasks') {
    renderTasksView();
  } else if (tabName === 'edits') {
    renderEditsView();
  }

  saveWebviewState();
}

export function handleBottomTabClick(tabName) {
  setCurrentBottomTab(tabName);

  // 更新 Tab 按钮状态
  document.querySelectorAll('.bottom-tab').forEach(tab => {
    tab.classList.toggle('active', tab.dataset.bottomTab === tabName);
  });

  // 渲染对应内容
  renderMainContent();
  saveWebviewState();
}

export function handleSettingsTabClick(tabName) {
  // 更新 Tab 按钮状态
  document.querySelectorAll('.settings-tab').forEach(tab => {
    tab.classList.toggle('active', tab.dataset.tab === tabName);
  });

  // 显示对应内容
  document.querySelectorAll('.settings-tab-content').forEach(content => {
    content.style.display = content.id === `settings-tab-${tabName}` ? 'block' : 'none';
  });
}

// ============================================
// 执行按钮处理
// ============================================

export function handleExecuteButtonClick() {
  // 如果正在处理中，点击按钮则打断
  if (isProcessing) {
    interruptCurrentOperation();
    return;
  }

  const input = document.getElementById('prompt-input');
  if (!input.value.trim() && attachedImages.length === 0) {
    return;
  }

  // 优先处理澄清请求
  if (hasPendingClarification()) {
    const answerText = input.value.trim();
    if (!answerText) {
      addSystemMessage('请输入澄清信息后再提交', 'warning');
      return;
    }
    handleClarificationAnswer(answerText, false);
    input.value = '';
    attachedImages.length = 0;
    renderImagePreviews();
    return;
  }

  // 处理 Worker 问题
  if (hasPendingWorkerQuestion()) {
    const answerText = input.value.trim();
    if (!answerText) {
      addSystemMessage('请输入回答后再提交', 'warning');
      return;
    }
    handleWorkerQuestionAnswer(answerText, false);
    input.value = '';
    attachedImages.length = 0;
    renderImagePreviews();
    return;
  }

  // 处理普通问题
  if (hasPendingQuestion()) {
    const answerText = input.value.trim();
    if (!answerText) {
      addSystemMessage('请先回答问题后再提交', 'warning');
      return;
    }
    handleQuestionAnswer(answerText, false);
    input.value = '';
    attachedImages.length = 0;
    renderImagePreviews();
    return;
  }

  // 处理执行计划确认
  if (hasPendingConfirmation()) {
    const userInput = input.value.trim().toLowerCase();
    const confirmKeywords = ['确认', '好的', '好', '是的', '是', 'yes', 'y', 'ok', '执行', '开始', '继续'];
    const cancelKeywords = ['取消', '不', '不要', '否', 'no', 'n', 'cancel', '停止'];

    const isConfirm = confirmKeywords.includes(userInput);
    const isCancel = cancelKeywords.includes(userInput);

    if (isConfirm || isCancel) {
      handlePlanConfirmation(isConfirm);
      input.value = '';
      attachedImages.length = 0;
      renderImagePreviews();
      return;
    }

    addSystemMessage('请明确回复"确认"或"取消"，或点击卡片按钮', 'warning');
    return;
  }

  // 正常执行任务
  const promptText = input.value.trim() || '请分析这张图片';
  const hasImages = attachedImages.length > 0;
  const selectedCli = document.getElementById('cli-selector')?.value || '';
  const isOrchestratorMode = !selectedCli;

  setProcessingActor(isOrchestratorMode ? 'orchestrator' : 'worker', selectedCli || 'claude');

  const imageDataUrls = hasImages ? attachedImages.map(img => img.dataUrl) : [];

  const userMsg = {
    role: 'user',
    content: promptText,
    time: new Date().toLocaleTimeString().slice(0, 5),
    timestamp: Date.now(),
    images: imageDataUrls
  };
  threadMessages.push(userMsg);

  renderMainContent();
  saveWebviewState();

  const mode = isOrchestratorMode ? 'agent' : 'ask';
  executeTask(promptText, hasImages ? imageDataUrls : null, mode, selectedCli || null);

  input.value = '';
  attachedImages.length = 0;
  renderImagePreviews();
}

// ============================================
// 输入框处理
// ============================================

export function handlePromptInputKeydown(e) {
  if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
    e.preventDefault();
    handleExecuteButtonClick();
  }
}

export function handlePromptInputPaste(e) {
  const items = e.clipboardData?.items;
  if (!items) return;

  for (const item of items) {
    if (item.type.startsWith('image/')) {
      e.preventDefault();
      const file = item.getAsFile();
      if (file) {
        handleImageFile(file);
      }
    }
  }
}

// ============================================
// 图片处理
// ============================================

export function handleImageFile(file) {
  if (!file.type.startsWith('image/')) {
    showToast('只支持图片文件', 'error');
    return;
  }

  const reader = new FileReader();
  reader.onload = (e) => {
    const dataUrl = e.target.result;
    attachedImages.push({
      name: file.name,
      dataUrl: dataUrl
    });
    renderImagePreviews();
  };
  reader.readAsDataURL(file);
}

export function handleAttachImageClick() {
  document.getElementById('image-file-input')?.click();
}

export function handleImageFileInputChange(e) {
  const files = e.target.files;
  if (files && files.length > 0) {
    for (const file of files) {
      handleImageFile(file);
    }
  }
  e.target.value = '';
}

export function handleRemoveImage(index) {
  attachedImages.splice(index, 1);
  renderImagePreviews();
}

// ============================================
// 拖拽处理
// ============================================

export function handleDragOver(e) {
  e.preventDefault();
  e.stopPropagation();
  e.currentTarget.classList.add('drag-over');
}

export function handleDragLeave(e) {
  e.preventDefault();
  e.stopPropagation();
  e.currentTarget.classList.remove('drag-over');
}

export function handleDrop(e) {
  e.preventDefault();
  e.stopPropagation();
  e.currentTarget.classList.remove('drag-over');

  const files = e.dataTransfer?.files;
  if (files && files.length > 0) {
    for (const file of files) {
      if (file.type.startsWith('image/')) {
        handleImageFile(file);
      }
    }
  }
}

// ============================================
// 会话管理
// ============================================

export function handleSessionSelect(sessionId) {
  if (sessionId === currentSessionId) return;

  loadSessionMessages(sessionId);

  // 关闭下拉菜单
  const dropdown = document.getElementById('session-dropdown');
  if (dropdown) {
    dropdown.style.display = 'none';
  }
}

export function handleNewSession() {
  postMessage({ type: 'newSession' });
}

export function handleRenameSession(sessionId) {
  const session = sessions.find(s => s.id === sessionId);
  if (!session) return;

  const newName = prompt('请输入新的会话名称:', session.name || '未命名会话');
  if (newName && newName.trim()) {
    postMessage({
      type: 'renameSession',
      sessionId: sessionId,
      name: newName.trim()
    });
  }
}

export function handleDeleteSession(sessionId) {
  const session = sessions.find(s => s.id === sessionId);
  if (!session) return;

  if (confirm(`确定要删除会话"${session.name || '未命名会话'}"吗？`)) {
    postMessage({
      type: 'deleteSession',
      sessionId: sessionId
    });
  }
}

export function handleExportSession(sessionId) {
  postMessage({
    type: 'exportSession',
    sessionId: sessionId
  });
}

// ============================================
// 变更管理
// ============================================

export function handleApproveChange(filePath) {
  postMessage({ type: 'approveChange', filePath });
}

export function handleRevertChange(filePath) {
  postMessage({ type: 'revertChange', filePath });
}

export function handleApproveAllChanges() {
  postMessage({ type: 'approveAllChanges' });
}

export function handleRevertAllChanges() {
  postMessage({ type: 'revertAllChanges' });
}

export function handleViewDiff(filePath) {
  postMessage({ type: 'viewDiff', filePath });
}

// ============================================
// 系统事件
// ============================================

export function handleVisibilityChange() {
  if (!document.hidden) {
    // 页面重新可见时，请求最新状态
    postMessage({ type: 'requestState' });
  }
}

export function handleWindowMessage(event) {
  const message = event.data;
  // 这里会调用 message-handler.js 中的处理函数
  // 具体逻辑在主初始化代码中
}

export function handleUnhandledRejection(event) {
  console.error('[Webview] Unhandled Promise Rejection:', event.reason);
  showToast('发生未处理的错误: ' + event.reason, 'error');
}

// ============================================
// 事件绑定初始化
// ============================================

export function initializeEventListeners() {
  // Top Tab 切换
  document.querySelectorAll('.top-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      handleTopTabClick(tab.dataset.tab);
    });
  });

  // Bottom Tab 切换
  document.querySelectorAll('.bottom-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      handleBottomTabClick(tab.dataset.bottomTab);
    });
  });

  // Settings Tab 切换
  document.querySelectorAll('.settings-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      handleSettingsTabClick(tab.dataset.tab);
    });
  });

  // 新建会话按钮
  const newSessionBtn = document.getElementById('new-session-btn');
  if (newSessionBtn) {
    newSessionBtn.addEventListener('click', () => {
      createNewSession();
    });
  }

  // 设置按钮
  const settingsBtn = document.getElementById('settings-btn');
  const settingsOverlay = document.getElementById('settings-overlay');
  if (settingsBtn && settingsOverlay) {
    settingsBtn.addEventListener('click', () => {
      settingsOverlay.style.display = 'flex';
      // 打开设置面板时初始化数据
      initializeSettingsPanel();
    });
  }

  // 关闭设置按钮
  const closeSettingsBtn = document.getElementById('settings-close-btn');
  if (closeSettingsBtn && settingsOverlay) {
    closeSettingsBtn.addEventListener('click', () => {
      settingsOverlay.style.display = 'none';
    });
  }

  // 点击设置 overlay 背景关闭
  if (settingsOverlay) {
    settingsOverlay.addEventListener('click', (e) => {
      if (e.target === settingsOverlay) {
        settingsOverlay.style.display = 'none';
      }
    });
  }

  // 执行按钮
  const executeBtn = document.getElementById('execute-btn');
  if (executeBtn) {
    executeBtn.addEventListener('click', handleExecuteButtonClick);
  }

  // 输入框
  const promptInput = document.getElementById('prompt-input');
  if (promptInput) {
    promptInput.addEventListener('keydown', handlePromptInputKeydown);
    promptInput.addEventListener('paste', handlePromptInputPaste);
  }

  // CLI 选择器
  const cliSelector = document.getElementById('cli-selector');
  if (cliSelector) {
    cliSelector.addEventListener('change', (e) => {
      postMessage({ type: 'selectCli', cli: e.target.value || null });
    });
  }

  // 交互模式选择器
  const modeSelector = document.getElementById('mode-selector');
  if (modeSelector) {
    modeSelector.addEventListener('change', (e) => {
      const mode = e.target.value;
      if (mode && mode !== currentInteractionMode) {
        currentInteractionMode = mode;
        postMessage({ type: 'setInteractionMode', mode: mode });
      }
    });
  }

  // 图片上传
  const attachImageBtn = document.getElementById('attach-image-btn');
  if (attachImageBtn) {
    attachImageBtn.addEventListener('click', handleAttachImageClick);
  }

  const imageFileInput = document.getElementById('image-file-input');
  if (imageFileInput) {
    imageFileInput.addEventListener('change', handleImageFileInputChange);
  }

  // Prompt 增强按钮
  const enhanceBtn = document.getElementById('enhance-btn');
  if (enhanceBtn) {
    enhanceBtn.addEventListener('click', () => {
      const input = document.getElementById('prompt-input');
      const prompt = input ? input.value.trim() : '';
      if (!prompt) {
        showToast('请先输入任务描述', 'warning');
        return;
      }
      const textSpan = enhanceBtn.querySelector('.enhance-text');
      enhanceBtn.classList.add('loading');
      enhanceBtn.disabled = true;
      if (textSpan) textSpan.textContent = '增强中';
      postMessage({ type: 'enhancePrompt', prompt: prompt });
    });
  }

  // 拖拽
  const inputWrapper = document.querySelector('.input-wrapper');
  if (inputWrapper) {
    inputWrapper.addEventListener('dragover', handleDragOver);
    inputWrapper.addEventListener('dragleave', handleDragLeave);
    inputWrapper.addEventListener('drop', handleDrop);
  }

  // 拖动调整输入框高度
  const resizeBar = document.getElementById('input-resize-bar');
  const inputBox = document.getElementById('prompt-input');
  if (resizeBar && inputWrapper && inputBox) {
    let isDragging = false;
    let startY = 0;
    let startHeight = 0;
    let rafId = null;

    function updateHeight(e) {
      if (!isDragging) return;
      const dy = startY - e.clientY;
      const newHeight = Math.max(60, Math.min(startHeight + dy, 300));
      inputBox.style.height = newHeight + 'px';
    }

    resizeBar.addEventListener('mousedown', (e) => {
      isDragging = true;
      startY = e.clientY;
      startHeight = inputBox.offsetHeight;
      inputWrapper.classList.add('resizing');
      document.body.style.cursor = 'ns-resize';
      document.body.style.userSelect = 'none';
      e.preventDefault();
      e.stopPropagation();
    });

    document.addEventListener('mousemove', (e) => {
      if (!isDragging) return;
      e.preventDefault();
      if (rafId) cancelAnimationFrame(rafId);
      rafId = requestAnimationFrame(() => updateHeight(e));
    });

    document.addEventListener('mouseup', () => {
      if (!isDragging) return;
      isDragging = false;
      inputWrapper.classList.remove('resizing');
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      if (rafId) {
        cancelAnimationFrame(rafId);
        rafId = null;
      }
    });

    resizeBar.addEventListener('selectstart', (e) => e.preventDefault());
  }

  // 计划确认 / 问题回答按钮委托
  document.body.addEventListener('click', (e) => {
    const btn = e.target.closest('.plan-confirm-btn');
    if (!btn) return;
    const action = btn.dataset.action;
    if (!action) return;
    handlePlanConfirmation(action === 'confirm');
  });

  document.body.addEventListener('click', (e) => {
    const btn = e.target.closest('.plan-start-btn');
    if (!btn) return;
    setProcessingState(true);
    const planId = btn.dataset.planId || '';
    const command = planId ? `/start-work ${planId}` : '/start-work';
    postMessage({ type: 'executeTask', prompt: command });
  });

  document.body.addEventListener('click', (e) => {
    const btn = e.target.closest('.question-btn');
    if (!btn) return;
    const action = btn.dataset.action;
    const card = btn.closest('.question-card');
    const textarea = card ? card.querySelector('.question-answer') : null;
    const answer = textarea ? textarea.value.trim() : '';
    if (action === 'submit') {
      if (!answer) {
        addSystemMessage('请先回答问题后再提交', 'warning');
        return;
      }
      handleQuestionAnswer(answer, false);
    } else if (action === 'cancel') {
      handleQuestionAnswer('', true);
    }
  });

  // MCP 管理
  const mcpAddBtn = document.getElementById('mcp-add-btn');
  if (mcpAddBtn) {
    mcpAddBtn.addEventListener('click', () => showMCPDialog());
  }

  // Skill 仓库管理
  const repoManageBtn = document.getElementById('repo-manage-btn');
  if (repoManageBtn) {
    repoManageBtn.addEventListener('click', () => showRepositoryManagementDialog());
  }

  // Skill 安装入口
  const skillAddBtn = document.getElementById('skill-add-btn');
  if (skillAddBtn) {
    skillAddBtn.addEventListener('click', () => showSkillLibraryDialog());
  }

  // Prompt 增强配置
  const promptEnhanceUrl = document.getElementById('prompt-enhance-url');
  const promptEnhanceKey = document.getElementById('prompt-enhance-key');
  const promptEnhanceEye = document.getElementById('prompt-enhance-eye');
  const promptEnhanceTest = document.getElementById('prompt-enhance-test');

  if (promptEnhanceUrl && promptEnhanceKey) {
    postMessage({ type: 'getPromptEnhanceConfig' });
    promptEnhanceUrl.addEventListener('change', savePromptEnhanceConfig);
    promptEnhanceKey.addEventListener('change', savePromptEnhanceConfig);
  }

  if (promptEnhanceEye && promptEnhanceKey) {
    promptEnhanceEye.addEventListener('click', () => {
      const isPassword = promptEnhanceKey.type === 'password';
      promptEnhanceKey.type = isPassword ? 'text' : 'password';
      const closed = promptEnhanceEye.querySelector('.eye-closed');
      const open = promptEnhanceEye.querySelector('.eye-open');
      if (closed) closed.style.display = isPassword ? 'none' : 'block';
      if (open) open.style.display = isPassword ? 'block' : 'none';
    });
  }

  if (promptEnhanceTest) {
    promptEnhanceTest.addEventListener('click', () => {
      const btn = promptEnhanceTest;
      btn.classList.add('loading');
      btn.disabled = true;
      postMessage({
        type: 'testPromptEnhance',
        baseUrl: promptEnhanceUrl ? promptEnhanceUrl.value : '',
        apiKey: promptEnhanceKey ? promptEnhanceKey.value : ''
      });
    });
  }

  // 系统事件
  document.addEventListener('visibilitychange', handleVisibilityChange);
  window.addEventListener('unhandledrejection', handleUnhandledRejection);

  // 全局快捷键
  document.addEventListener('keydown', (e) => {
    // Cmd/Ctrl + Enter 执行
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      handleExecuteButtonClick();
    }
    if (e.key === 'Escape' && isProcessing) {
      e.preventDefault();
      interruptCurrentOperation();
    }
  });

  console.log('[EventHandlers] 事件监听器初始化完成');
}

// ============================================
// 全局函数（供 HTML onclick 使用）
// ============================================

// 这些函数需要挂载到 window 对象上，供 HTML 中的 onclick 属性调用
window.approveChange = handleApproveChange;
window.revertChange = handleRevertChange;
window.approveAllChanges = handleApproveAllChanges;
window.revertAllChanges = handleRevertAllChanges;
window.viewDiff = handleViewDiff;
window.removeImage = handleRemoveImage;
window.toggleDependencyPanel = toggleDependencyPanel;
window.showRepositoryManagementDialog = showRepositoryManagementDialog;
window.closeRepositoryManagementDialog = closeRepositoryManagementDialog;
window.addRepositoryFromDialog = addRepositoryFromDialog;
window.refreshRepositoryInDialog = refreshRepositoryInDialog;
window.deleteRepositoryFromDialog = deleteRepositoryFromDialog;
window.showSkillLibraryDialog = showSkillLibraryDialog;
window.closeSkillLibraryDialog = closeSkillLibraryDialog;
window.installSkill = installSkill;

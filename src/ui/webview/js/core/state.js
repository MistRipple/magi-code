// 全局状态管理
// 此文件包含所有全局状态变量和状态持久化逻辑

// VSCode API
export const vscode = typeof acquireVsCodeApi === 'function'
  ? acquireVsCodeApi()
  : {
      postMessage: () => {},
      getState: () => ({}),
      setState: () => {}
    };

// 从 VSCode 状态恢复
const previousState = vscode.getState() || {};

// Tab 状态
export let currentTopTab = 'thread';
export let currentBottomTab = 'thread';

// 消息状态
export let threadMessages = previousState.threadMessages || [];
export let cliOutputs = previousState.cliOutputs || { claude: [], codex: [], gemini: [] };

// 会话状态
export let sessions = previousState.sessions || [];
const injectedSessionId = '{{initialSessionId}}';
export let currentSessionId = previousState.currentSessionId || (injectedSessionId || null);

// 变更和任务状态
export let pendingChanges = previousState.pendingChanges || [];
export let tasks = previousState.tasks || [];

// 处理状态
export let isProcessing = previousState.isProcessing || false;
export let thinkingStartAt = previousState.thinkingStartAt || null;
export let localProcessingUntil = 0;
export let streamingHintTimer = null;
export let processingActor = previousState.processingActor || { source: 'orchestrator', cli: 'claude' };

// 后端下发的完整状态（用于 UI 渲染）
export let appState = null;

// 依赖分析状态
export let currentDependencyAnalysis = null;
export let isDependencyPanelExpanded = false;

// 滚动状态
export let scrollPositions = previousState.scrollPositions || { thread: 0, claude: 0, codex: 0, gemini: 0 };
export let autoScrollEnabled = previousState.autoScrollEnabled || { thread: true, claude: true, codex: true, gemini: true };
export let hasInitialRender = false;

// 消息列表限制
const MAX_THREAD_MESSAGES = 500;
const MAX_CLI_MESSAGES = 200;

// 裁剪消息列表
export function trimMessageLists() {
  if (threadMessages.length > MAX_THREAD_MESSAGES) {
    threadMessages = threadMessages.slice(-MAX_THREAD_MESSAGES);
  }
  ['claude', 'codex', 'gemini'].forEach(cli => {
    if (cliOutputs[cli] && cliOutputs[cli].length > MAX_CLI_MESSAGES) {
      cliOutputs[cli] = cliOutputs[cli].slice(-MAX_CLI_MESSAGES);
    }
  });
}

// 保存状态到 VSCode
export function saveWebviewState() {
  trimMessageLists();
  vscode.setState({
    currentTopTab,
    currentBottomTab,
    threadMessages,
    cliOutputs,
    sessions,
    currentSessionId,
    pendingChanges,
    tasks,
    isProcessing,
    thinkingStartAt,
    processingActor,
    scrollPositions,
    autoScrollEnabled
  });
}

// 状态更新函数
export function setCurrentTopTab(tab) {
  currentTopTab = tab;
  saveWebviewState();
}

export function setCurrentBottomTab(tab) {
  currentBottomTab = tab;
  saveWebviewState();
}

export function setCurrentSessionId(id) {
  currentSessionId = id;
  saveWebviewState();
}

export function setIsProcessing(value) {
  isProcessing = value;
  saveWebviewState();
}

export function setThinkingStartAt(value) {
  thinkingStartAt = value;
  saveWebviewState();
}

export function setProcessingActor(source, cli) {
  if (source && typeof source === 'object') {
    processingActor = source;
  } else {
    processingActor = {
      source: source || 'orchestrator',
      cli: cli || 'claude'
    };
  }
  saveWebviewState();
}

export function setAppState(nextState) {
  appState = nextState || null;
}

export function addThreadMessage(message) {
  threadMessages.push(message);
  saveWebviewState();
}

export function addCliOutput(cli, message) {
  if (!cliOutputs[cli]) {
    cliOutputs[cli] = [];
  }
  cliOutputs[cli].push(message);
  saveWebviewState();
}

export function clearThreadMessages() {
  threadMessages = [];
  saveWebviewState();
}

export function clearCliOutputs() {
  cliOutputs = { claude: [], codex: [], gemini: [] };
  saveWebviewState();
}

// 更新 sessions
export function updateSessions(newSessions) {
  sessions.length = 0;
  sessions.push(...newSessions);
  saveWebviewState();
}

// 更新 pendingChanges
export function updatePendingChanges(newChanges) {
  pendingChanges.length = 0;
  pendingChanges.push(...newChanges);
  saveWebviewState();
}

// 更新 tasks
export function updateTasks(newTasks) {
  tasks.length = 0;
  tasks.push(...newTasks);
  saveWebviewState();
}

// 处理宽限期
export function setLocalProcessingGrace(ms) {
  localProcessingUntil = Date.now() + ms;
}

export function hasLocalProcessingGrace() {
  return localProcessingUntil > 0 && Date.now() < localProcessingUntil;
}

export function clearLocalProcessingGrace() {
  localProcessingUntil = 0;
}

// 流式提示计时器管理
export function stopStreamingHintTimer() {
  if (!streamingHintTimer) return;
  clearInterval(streamingHintTimer);
  streamingHintTimer = null;
}

export function setStreamingHintTimer(timer) {
  streamingHintTimer = timer;
}

// 滚动位置
export function saveScrollPosition() {
  const mainContent = document.getElementById('main-content');
  if (mainContent) {
    scrollPositions[currentBottomTab] = mainContent.scrollTop;
  }
}

export function setScrollPosition(tab, position) {
  scrollPositions[tab] = position;
  saveWebviewState();
}

export function setAutoScrollEnabled(tab, enabled) {
  autoScrollEnabled[tab] = enabled;
  saveWebviewState();
}

export function setDependencyAnalysis(data) {
  currentDependencyAnalysis = data;
}

export function setDependencyPanelExpanded(expanded) {
  isDependencyPanelExpanded = expanded;
}

// 恢复 Webview 状态（从 VSCode 持久化存储中恢复）
export function restoreWebviewState() {
  // previousState 已经在文件开头从 vscode.getState() 加载
  // 这里只需要确保状态已经正确应用
  console.log('[State] 状态已从持久化存储恢复');
}

// 附加的图片（用于输入框）
export let attachedImages = [];

// 导出完整的 state 对象供调试使用
export const state = {
  get currentTopTab() { return currentTopTab; },
  get currentBottomTab() { return currentBottomTab; },
  get threadMessages() { return threadMessages; },
  get cliOutputs() { return cliOutputs; },
  get sessions() { return sessions; },
  get currentSessionId() { return currentSessionId; },
  get pendingChanges() { return pendingChanges; },
  get isProcessing() { return isProcessing; },
  get attachedImages() { return attachedImages; },
  get appState() { return appState; }
};

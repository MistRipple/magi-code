/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  AgentOutputs,
  Session,
  TabType,
  ProcessingActor,
  ScrollPositions,
  AutoScrollConfig,
  AppState,
  WebviewPersistedState,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';

// ============ 状态定义 ============

// Tab 状态
let currentTopTab = $state<TabType>('thread');
let currentBottomTab = $state<TabType>('thread');

// 消息状态
let threadMessages = $state<Message[]>([]);
let agentOutputs = $state<AgentOutputs>({
  claude: [],
  codex: [],
  gemini: [],
});

// 会话状态
let sessions = $state<Session[]>([]);
let currentSessionId = $state<string | null>(null);

// 处理状态
let isProcessing = $state(false);
let thinkingStartAt = $state<number | null>(null);
let processingActor = $state<ProcessingActor>({
  source: 'orchestrator',
  agent: 'claude',
});

// 后端下发的完整状态
let appState = $state<AppState | null>(null);

// 滚动状态
let scrollPositions = $state<ScrollPositions>({
  thread: 0,
  claude: 0,
  codex: 0,
  gemini: 0,
});
let autoScrollEnabled = $state<AutoScrollConfig>({
  thread: true,
  claude: true,
  codex: true,
  gemini: true,
});

// 消息列表限制
const MAX_THREAD_MESSAGES = 500;
const MAX_AGENT_MESSAGES = 200;

// 新增状态：任务、变更、阶段、Toast、模型状态
let tasks = $state<Array<{ id: string; name: string; description?: string; status: string }>>([]);
let edits = $state<Array<{ path: string; type?: string; additions?: number; deletions?: number }>>([]);
let currentPhase = $state(0);
let toasts = $state<Array<{ id: string; type: string; title?: string; message: string }>>([]);
let modelStatus = $state<Record<string, string>>({
  claude: 'unavailable',
  codex: 'unavailable',
  gemini: 'unavailable',
});

// ============ 导出 Getter ============

export function getState() {
  return {
    get currentTopTab() { return currentTopTab; },
    get currentBottomTab() { return currentBottomTab; },
    get threadMessages() { return threadMessages; },
    get agentOutputs() { return agentOutputs; },
    get sessions() { return sessions; },
    get currentSessionId() { return currentSessionId; },
    get isProcessing() { return isProcessing; },
    get thinkingStartAt() { return thinkingStartAt; },
    get processingActor() { return processingActor; },
    get appState() { return appState; },
    get scrollPositions() { return scrollPositions; },
    get autoScrollEnabled() { return autoScrollEnabled; },
    // 新增
    get tasks() { return tasks; },
    set tasks(v) { tasks = v; },
    get edits() { return edits; },
    set edits(v) { edits = v; },
    get currentPhase() { return currentPhase; },
    set currentPhase(v) { currentPhase = v; },
    get toasts() { return toasts; },
    set toasts(v) { toasts = v; },
    get modelStatus() { return modelStatus; },
    set modelStatus(v) { modelStatus = v; },
  };
}

// ============ 状态更新函数 ============

// 裁剪消息列表
function trimMessageLists() {
  if (threadMessages.length > MAX_THREAD_MESSAGES) {
    threadMessages = threadMessages.slice(-MAX_THREAD_MESSAGES);
  }
  (['claude', 'codex', 'gemini'] as const).forEach((agent) => {
    if (agentOutputs[agent].length > MAX_AGENT_MESSAGES) {
      agentOutputs[agent] = agentOutputs[agent].slice(-MAX_AGENT_MESSAGES);
    }
  });
}

// 保存状态到 VS Code
function saveWebviewState() {
  trimMessageLists();
  const state: WebviewPersistedState = {
    currentTopTab,
    currentBottomTab,
    threadMessages,
    agentOutputs,
    sessions,
    currentSessionId,
    scrollPositions,
    autoScrollEnabled,
  };
  vscode.setState(state);
}

// Tab 操作
export function setCurrentTopTab(tab: TabType) {
  currentTopTab = tab;
  saveWebviewState();
}

export function setCurrentBottomTab(tab: TabType) {
  currentBottomTab = tab;
  saveWebviewState();
}

// 会话操作
export function setCurrentSessionId(id: string | null) {
  currentSessionId = id;
  saveWebviewState();
}

export function updateSessions(newSessions: Session[]) {
  sessions = [...newSessions];
  saveWebviewState();
}

// 处理状态操作
export function setIsProcessing(value: boolean) {
  isProcessing = value;
}

export function setThinkingStartAt(value: number | null) {
  thinkingStartAt = value;
}

export function setProcessingActor(source: string, agent?: string) {
  processingActor = {
    source: source as ProcessingActor['source'],
    agent: (agent || 'claude') as ProcessingActor['agent'],
  };
}

export function setAppState(nextState: AppState | null) {
  appState = nextState;
}

// 消息操作
export function addThreadMessage(message: Message) {
  threadMessages = [...threadMessages, message];
  saveWebviewState();
}

export function updateThreadMessage(messageId: string, updates: Partial<Message>) {
  const index = threadMessages.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    threadMessages[index] = { ...threadMessages[index], ...updates };
    // 不触发保存，由流式管理器批量保存
  }
}

export function clearThreadMessages() {
  threadMessages = [];
  saveWebviewState();
}

// 导出状态初始化
export function initializeState() {
  const persisted = vscode.getState<WebviewPersistedState>();
  if (persisted) {
    currentTopTab = persisted.currentTopTab || 'thread';
    currentBottomTab = persisted.currentBottomTab || 'thread';
    threadMessages = persisted.threadMessages || [];
    agentOutputs = persisted.agentOutputs || { claude: [], codex: [], gemini: [] };
    sessions = persisted.sessions || [];
    currentSessionId = persisted.currentSessionId || null;
    scrollPositions = persisted.scrollPositions || { thread: 0, claude: 0, codex: 0, gemini: 0 };
    autoScrollEnabled = persisted.autoScrollEnabled || { thread: true, claude: true, codex: true, gemini: true };
  }
}


/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  AgentOutputs,
  AgentType,
  MissionPlan,
  Session,
  TabType,
  ProcessingActor,
  ScrollPositions,
  AutoScrollConfig,
  AppState,
  WebviewPersistedState,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray } from '../lib/utils';

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

const MAX_PERSISTED_ARRAY_LENGTH = 10000;

function isValidPersistedArray(value: unknown, max: number): value is unknown[] {
  if (!Array.isArray(value)) return false;
  const length = value.length;
  if (!Number.isFinite(length) || length < 0 || length > max) return false;
  return true;
}

function resetPersistedState() {
  currentTopTab = 'thread';
  currentBottomTab = 'thread';
  threadMessages = [];
  agentOutputs = { claude: [], codex: [], gemini: [] };
  sessions = [];
  currentSessionId = null;
  scrollPositions = { thread: 0, claude: 0, codex: 0, gemini: 0 };
  autoScrollEnabled = { thread: true, claude: true, codex: true, gemini: true };
  vscode.setState(null);
}

function isValidMessageSource(message: Message | null | undefined): boolean {
  if (!message || typeof message !== 'object') return false;
  const source = (message as Message).source;
  return typeof source === 'string' && source.length > 0;
}

function hasInvalidMessageSource(messages: Message[]): boolean {
  return messages.some((msg) => !isValidMessageSource(msg));
}

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

// 交互请求状态
let pendingConfirmation = $state<{ plan: unknown; formattedPlan?: string } | null>(null);
let pendingRecovery = $state<{ taskId: string; error: unknown; canRetry: boolean; canRollback: boolean } | null>(null);
let pendingQuestion = $state<{ questions: string[]; plan?: unknown } | null>(null);
let pendingClarification = $state<{ questions: string[]; context?: string; ambiguityScore?: number; originalPrompt?: string } | null>(null);
let pendingWorkerQuestion = $state<{ workerId: string; question: string; context?: string; options?: unknown } | null>(null);
let pendingToolAuthorization = $state<{ toolName: string; toolArgs: unknown } | null>(null);
let missionPlan = $state<MissionPlan | null>(null);

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
    get pendingConfirmation() { return pendingConfirmation; },
    set pendingConfirmation(v) { pendingConfirmation = v; },
    get pendingRecovery() { return pendingRecovery; },
    set pendingRecovery(v) { pendingRecovery = v; },
    get pendingQuestion() { return pendingQuestion; },
    set pendingQuestion(v) { pendingQuestion = v; },
    get pendingClarification() { return pendingClarification; },
    set pendingClarification(v) { pendingClarification = v; },
    get pendingWorkerQuestion() { return pendingWorkerQuestion; },
    set pendingWorkerQuestion(v) { pendingWorkerQuestion = v; },
    get pendingToolAuthorization() { return pendingToolAuthorization; },
    set pendingToolAuthorization(v) { pendingToolAuthorization = v; },
    get missionPlan() { return missionPlan; },
    set missionPlan(v) { missionPlan = v; },
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
  sessions = ensureArray(newSessions) as Session[];
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

export function setMissionPlan(plan: MissionPlan | null) {
  missionPlan = plan;
}

// 消息操作
export function addThreadMessage(message: Message) {
  // 完全重建数组以确保响应式更新
  const safeMessage = JSON.parse(JSON.stringify(message)) as Message;
  threadMessages = [...threadMessages, safeMessage];
  saveWebviewState();
}

export function updateThreadMessage(messageId: string, updates: Partial<Message>) {
  const index = threadMessages.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    // 必须完全重建数组，不能直接修改索引
    // 使用 JSON 序列化确保脱离响应式代理
    const safeUpdates = JSON.parse(JSON.stringify(updates)) as Partial<Message>;
    const newMessages = threadMessages.map((msg, i) => {
      if (i === index) {
        return { ...msg, ...safeUpdates };
      }
      return msg;
    });
    threadMessages = newMessages;
    // 不触发保存，由流式管理器批量保存
  }
}

export function clearThreadMessages() {
  threadMessages = [];
  saveWebviewState();
}

export function addAgentMessage(agent: AgentType, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(message)) as Message;
  agentOutputs = {
    ...agentOutputs,
    [agent]: [...agentOutputs[agent], safeMessage],
  };
  saveWebviewState();
}

export function updateAgentMessage(agent: AgentType, messageId: string, updates: Partial<Message>) {
  const list = agentOutputs[agent];
  const index = list.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    const safeUpdates = JSON.parse(JSON.stringify(updates)) as Partial<Message>;
    const next = list.map((msg, i) => (i === index ? { ...msg, ...safeUpdates } : msg));
    agentOutputs = { ...agentOutputs, [agent]: next };
    // 不触发保存，由流式管理器批量保存
  }
}

export function clearAgentMessages(agent: AgentType) {
  agentOutputs = { ...agentOutputs, [agent]: [] };
  saveWebviewState();
}

// 导出状态初始化
export function initializeState() {
  const persisted = vscode.getState<WebviewPersistedState>();
  if (persisted) {
    const validThread = isValidPersistedArray(persisted.threadMessages, MAX_PERSISTED_ARRAY_LENGTH);
    const validClaude = isValidPersistedArray(persisted.agentOutputs?.claude, MAX_PERSISTED_ARRAY_LENGTH);
    const validCodex = isValidPersistedArray(persisted.agentOutputs?.codex, MAX_PERSISTED_ARRAY_LENGTH);
    const validGemini = isValidPersistedArray(persisted.agentOutputs?.gemini, MAX_PERSISTED_ARRAY_LENGTH);
    const validSessions = isValidPersistedArray(persisted.sessions, MAX_PERSISTED_ARRAY_LENGTH);
    if (!validThread || !validClaude || !validCodex || !validGemini || !validSessions) {
      resetPersistedState();
      return;
    }
    currentTopTab = persisted.currentTopTab || 'thread';
    currentBottomTab = persisted.currentBottomTab || 'thread';
    threadMessages = ensureArray<Message>(persisted.threadMessages);
    agentOutputs = {
      claude: ensureArray<Message>(persisted.agentOutputs?.claude),
      codex: ensureArray<Message>(persisted.agentOutputs?.codex),
      gemini: ensureArray<Message>(persisted.agentOutputs?.gemini),
    };
    if (
      hasInvalidMessageSource(threadMessages) ||
      hasInvalidMessageSource(agentOutputs.claude) ||
      hasInvalidMessageSource(agentOutputs.codex) ||
      hasInvalidMessageSource(agentOutputs.gemini)
    ) {
      resetPersistedState();
      return;
    }
    sessions = ensureArray<Session>(persisted.sessions);
    currentSessionId = persisted.currentSessionId || null;
    scrollPositions = persisted.scrollPositions || { thread: 0, claude: 0, codex: 0, gemini: 0 };
    autoScrollEnabled = persisted.autoScrollEnabled || { thread: true, claude: true, codex: true, gemini: true };
  }
}

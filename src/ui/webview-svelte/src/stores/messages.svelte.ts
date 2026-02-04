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
  ContentBlock,
  ScrollPositions,
  AutoScrollConfig,
  AppState,
  WebviewPersistedState,
  WaveState,
  WorkerSessionState,
  RequestResponseBinding,
  ModelStatusMap,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray } from '../lib/utils';

// ============ 状态定义 ============
// 🔧 修复：使用对象属性模式确保跨模块响应式正常工作
// Svelte 5 官方推荐：导出对象并修改其属性，而非重新赋值独立变量

/**
 * 核心消息状态
 * 使用对象属性模式确保跨模块响应式追踪
 */
export const messagesState = $state({
  // Tab 状态
  currentTopTab: 'thread' as TabType,
  currentBottomTab: 'thread' as TabType,

  // 消息状态
  threadMessages: [] as Message[],
  agentOutputs: {
    claude: [],
    codex: [],
    gemini: [],
  } as AgentOutputs,

  // 会话状态
  sessions: [] as Session[],
  currentSessionId: null as string | null,

  // 处理状态
  isProcessing: false,
  backendProcessing: false,
  activeMessageIds: new Set<string>(),
  pendingRequests: new Set<string>(),
  thinkingStartAt: null as number | null,
  processingActor: {
    source: 'orchestrator',
    agent: 'claude',
  } as ProcessingActor,

  // 后端下发的完整状态
  appState: null as AppState | null,

  // 滚动状态
  scrollPositions: {
    thread: 0,
    claude: 0,
    codex: 0,
    gemini: 0,
  } as ScrollPositions,
  autoScrollEnabled: {
    thread: true,
    claude: true,
    codex: true,
    gemini: true,
  } as AutoScrollConfig,
});

// 🔧 兼容层：保留原有的独立变量引用（逐步迁移）
// 这些变量现在指向 messagesState 的属性
let currentTopTab = $derived(messagesState.currentTopTab);
let currentBottomTab = $derived(messagesState.currentBottomTab);
let threadMessages = $derived(messagesState.threadMessages);
let agentOutputs = $derived(messagesState.agentOutputs);
let sessions = $derived(messagesState.sessions);
let currentSessionId = $derived(messagesState.currentSessionId);
let isProcessing = $derived(messagesState.isProcessing);
let backendProcessing = $derived(messagesState.backendProcessing);
let activeMessageIds = $derived(messagesState.activeMessageIds);
let pendingRequests = $derived(messagesState.pendingRequests);
let thinkingStartAt = $derived(messagesState.thinkingStartAt);
let processingActor = $derived(messagesState.processingActor);
let appState = $derived(messagesState.appState);
let scrollPositions = $derived(messagesState.scrollPositions);
let autoScrollEnabled = $derived(messagesState.autoScrollEnabled);

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
let edits = $state<Array<{ filePath: string; type?: string; additions?: number; deletions?: number; contributors?: string[]; workerId?: string }>>([]);
let toasts = $state<Array<{ id: string; type: string; title?: string; message: string }>>([]);
let modelStatus = $state<ModelStatusMap>({
  claude: { status: 'checking' },
  codex: { status: 'checking' },
  gemini: { status: 'checking' },
  orchestrator: { status: 'checking' },
  compressor: { status: 'checking' },
});
let interactionMode = $state<'ask' | 'auto'>('auto');

// Worker 执行状态：idle | executing | completed | failed
let workerExecutionStatus = $state<Record<string, 'idle' | 'executing' | 'completed' | 'failed'>>({
  claude: 'idle',
  codex: 'idle',
  gemini: 'idle',
});

function sanitizeMessageBlocks(blocks: unknown): ContentBlock[] {
  const list = ensureArray(blocks);
  const invalid = list.filter(
    (block) => !block || typeof block !== 'object' || !('type' in (block as Record<string, unknown>))
  );
  if (invalid.length > 0) {
    throw new Error('[MessagesStore] 消息块无效');
  }
  return list as ContentBlock[];
}

function normalizePersistedMessages(messages: Message[] | undefined): Message[] {
  const seen = new Set<string>();
  const normalized: Message[] = [];
  for (const msg of ensureArray<Message>(messages)) {
    if (!msg || typeof msg !== 'object') {
      throw new Error('[MessagesStore] 持久化消息包含无效对象');
    }
    const id = typeof msg.id === 'string' && msg.id.trim().length > 0 ? msg.id.trim() : '';
    if (!id) {
      throw new Error('[MessagesStore] 持久化消息缺少 id');
    }
    if (seen.has(id)) {
      throw new Error(`[MessagesStore] 持久化消息 id 重复: ${id}`);
    }
    seen.add(id);
    const blocks = sanitizeMessageBlocks(msg.blocks);
    normalized.push({ ...msg, id, blocks: blocks.length > 0 ? blocks : undefined });
  }
  return normalized;
}

function normalizeIncomingMessage(message: Message): Message {
  if (!message || typeof message !== 'object') {
    throw new Error('[MessagesStore] 输入消息无效');
  }
  const id = typeof message.id === 'string' && message.id.trim().length > 0 ? message.id.trim() : '';
  if (!id) {
    throw new Error('[MessagesStore] 输入消息缺少 id');
  }
  const blocks = sanitizeMessageBlocks(message.blocks);
  return { ...message, id, blocks: blocks.length > 0 ? blocks : undefined };
}

function normalizeMissionPlan(plan: MissionPlan | null): MissionPlan | null {
  if (!plan || typeof plan !== 'object') return null;
  const assignmentSeen = new Set<string>();
  const assignments = ensureArray(plan.assignments)
    .filter((assignment: any) => assignment && typeof assignment === 'object')
    .map((assignment: any) => {
      const assignmentId = typeof assignment.id === 'string' && assignment.id.trim() ? assignment.id.trim() : '';
      if (!assignmentId) {
        throw new Error('[MessagesStore] MissionPlan assignment 缺少 id');
      }
      if (assignmentSeen.has(assignmentId)) {
        throw new Error(`[MessagesStore] MissionPlan assignment id 重复: ${assignmentId}`);
      }
      assignmentSeen.add(assignmentId);
      const todoSeen = new Set<string>();
      const todos = ensureArray(assignment.todos)
        .filter((todo: any) => todo && typeof todo === 'object')
        .map((todo: any) => {
          const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
          if (!todoId) {
            throw new Error('[MessagesStore] MissionPlan todo 缺少 id');
          }
          if (todoSeen.has(todoId)) {
            throw new Error(`[MessagesStore] MissionPlan todo id 重复: ${todoId}`);
          }
          todoSeen.add(todoId);
          return { ...todo, id: todoId, assignmentId };
        });
      return { ...assignment, id: assignmentId, todos };
    });
  return { ...plan, missionId: plan.missionId || '', assignments };
}

// 交互请求状态
let pendingConfirmation = $state<{ plan: unknown; formattedPlan?: string } | null>(null);
let pendingRecovery = $state<{ taskId: string; error: unknown; canRetry: boolean; canRollback: boolean } | null>(null);
let pendingQuestion = $state<{ questions: string[]; plan?: unknown } | null>(null);
let pendingClarification = $state<{ questions: string[]; context?: string; ambiguityScore?: number; originalPrompt?: string } | null>(null);
let pendingWorkerQuestion = $state<{ workerId: string; question: string; context?: string; options?: unknown } | null>(null);
let pendingToolAuthorization = $state<{ toolName: string; toolArgs: unknown } | null>(null);
let missionPlan = $state<MissionPlan | null>(null);

// Wave 执行状态（提案 4.6）
let waveState = $state<WaveState | null>(null);

// Worker Session 状态（提案 4.1）
let workerSessions = $state<Map<string, WorkerSessionState>>(new Map());

// 请求-响应绑定状态（消息响应流设计）
let requestBindings = $state<Map<string, RequestResponseBinding>>(new Map());

// 请求超时时间（30秒）

// ============ 直接导出响应式状态（Svelte 5 推荐方式）============
// 🔧 修复响应式追踪问题：通过 messagesState 对象属性访问
// Svelte 5 官方推荐：导出对象属性读取，确保响应式追踪正常

export function getThreadMessages() {
  return messagesState.threadMessages;
}

export function getAgentOutputs() {
  return messagesState.agentOutputs;
}

export function getCurrentBottomTab() {
  return messagesState.currentBottomTab;
}

export function getCurrentTopTab() {
  return messagesState.currentTopTab;
}

export function getIsProcessing() {
  return messagesState.isProcessing;
}

export function getThinkingStartAt() {
  return messagesState.thinkingStartAt;
}

export function getProcessingActor() {
  return messagesState.processingActor;
}

export function getSessions() {
  return messagesState.sessions;
}

export function getCurrentSessionId() {
  return messagesState.currentSessionId;
}

export function getAppState() {
  return messagesState.appState;
}

export function getScrollPositions() {
  return messagesState.scrollPositions;
}

export function getAutoScrollEnabled() {
  return messagesState.autoScrollEnabled;
}

export function getTasks() {
  return tasks;
}

export function getEdits() {
  return edits;
}

export function getToasts() {
  return toasts;
}

export function getModelStatus() {
  return modelStatus;
}

export function getInteractionMode() {
  return interactionMode;
}

export function getWorkerExecutionStatus() {
  return workerExecutionStatus;
}

export function getPendingConfirmation() {
  return pendingConfirmation;
}

export function getPendingRecovery() {
  return pendingRecovery;
}

export function getPendingQuestion() {
  return pendingQuestion;
}

export function getPendingClarification() {
  return pendingClarification;
}

export function getPendingWorkerQuestion() {
  return pendingWorkerQuestion;
}

export function getPendingToolAuthorization() {
  return pendingToolAuthorization;
}

export function getMissionPlan() {
  return missionPlan;
}

export function getWaveState() {
  return waveState;
}

export function getWorkerSessions() {
  return workerSessions;
}

// ============ 兼容旧版 getState()（逐步迁移）============
// ⚠️ 已废弃：此函数返回的对象无法被 Svelte 5 正确追踪
// 请使用上面的独立 getter 函数或直接使用 messagesState

export function getState() {
  return {
    get currentTopTab() { return messagesState.currentTopTab; },
    get currentBottomTab() { return messagesState.currentBottomTab; },
    get threadMessages() { return messagesState.threadMessages; },
    get agentOutputs() { return messagesState.agentOutputs; },
    get sessions() { return messagesState.sessions; },
    get currentSessionId() { return messagesState.currentSessionId; },
    get isProcessing() { return messagesState.isProcessing; },
    get thinkingStartAt() { return messagesState.thinkingStartAt; },
    get processingActor() { return messagesState.processingActor; },
    get appState() { return messagesState.appState; },
    get scrollPositions() { return messagesState.scrollPositions; },
    get autoScrollEnabled() { return messagesState.autoScrollEnabled; },
    // 新增
    get tasks() { return tasks; },
    set tasks(v) { tasks = v; },
    get edits() { return edits; },
    set edits(v) { edits = v; },
    get toasts() { return toasts; },
    set toasts(v) { toasts = v; },
    get modelStatus() { return modelStatus; },
    set modelStatus(v) { modelStatus = v; },
    get interactionMode() { return interactionMode; },
    set interactionMode(v) { interactionMode = v; },
    // Worker 状态
    get workerExecutionStatus() { return workerExecutionStatus; },
    set workerExecutionStatus(v) { workerExecutionStatus = v; },
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
    // Wave 状态（提案 4.6）
    get waveState() { return waveState; },
    set waveState(v) { waveState = v; },
    // Worker Session 状态（提案 4.1）
    get workerSessions() { return workerSessions; },
    set workerSessions(v) { workerSessions = v; },
  };
}

// ============ 状态更新函数 ============

// 裁剪消息列表
function trimMessageLists() {
  if (messagesState.threadMessages.length > MAX_THREAD_MESSAGES) {
    messagesState.threadMessages = messagesState.threadMessages.slice(-MAX_THREAD_MESSAGES);
  }
  (['claude', 'codex', 'gemini'] as const).forEach((agent) => {
    if (messagesState.agentOutputs[agent].length > MAX_AGENT_MESSAGES) {
      messagesState.agentOutputs = {
        ...messagesState.agentOutputs,
        [agent]: messagesState.agentOutputs[agent].slice(-MAX_AGENT_MESSAGES),
      };
    }
  });
}

// 保存状态到 VS Code
function saveWebviewState() {
  trimMessageLists();
  const state: WebviewPersistedState = {
    currentTopTab: messagesState.currentTopTab,
    currentBottomTab: messagesState.currentBottomTab,
    threadMessages: messagesState.threadMessages,
    agentOutputs: messagesState.agentOutputs,
    sessions: messagesState.sessions,
    currentSessionId: messagesState.currentSessionId,
    scrollPositions: messagesState.scrollPositions,
    autoScrollEnabled: messagesState.autoScrollEnabled,
  };
  vscode.setState(state);
}

// Tab 操作
export function setCurrentTopTab(tab: TabType) {
  messagesState.currentTopTab = tab;
  saveWebviewState();
}

export function setCurrentBottomTab(tab: TabType) {
  messagesState.currentBottomTab = tab;
  saveWebviewState();
}

// 会话操作
export function setCurrentSessionId(id: string | null) {
  messagesState.currentSessionId = id;
  saveWebviewState();
}

export function updateSessions(newSessions: Session[]) {
  const seen = new Set<string>();
  messagesState.sessions = ensureArray<Session>(newSessions)
    .filter((session): session is Session => !!session && typeof session === 'object' && typeof session.id === 'string' && session.id.trim().length > 0)
    .filter((session) => {
      if (seen.has(session.id)) return false;
      seen.add(session.id);
      return true;
    });
  saveWebviewState();
}

// 处理状态操作
export function setIsProcessing(value: boolean) {
  messagesState.backendProcessing = value;
  updateProcessingState();
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
  missionPlan = normalizeMissionPlan(plan);
}

// Worker 执行状态操作
export function setWorkerExecutionStatus(
  worker: 'claude' | 'codex' | 'gemini',
  status: 'idle' | 'executing' | 'completed' | 'failed'
) {
  workerExecutionStatus = { ...workerExecutionStatus, [worker]: status };

  // 完成或失败状态 2 秒后自动重置为 idle
  if (status === 'completed' || status === 'failed') {
    setTimeout(() => {
      workerExecutionStatus = { ...workerExecutionStatus, [worker]: 'idle' };
    }, 2000);
  }
}

function updateProcessingState() {
  messagesState.isProcessing = messagesState.backendProcessing || messagesState.activeMessageIds.size > 0 || messagesState.pendingRequests.size > 0;
}

export function markMessageActive(id: string) {
  if (!id) return;
  if (!messagesState.activeMessageIds.has(id)) {
    const next = new Set(messagesState.activeMessageIds);
    next.add(id);
    messagesState.activeMessageIds = next;
    updateProcessingState();
  }
}

export function markMessageComplete(id: string) {
  if (!id) return;
  if (messagesState.activeMessageIds.has(id)) {
    const next = new Set(messagesState.activeMessageIds);
    next.delete(id);
    messagesState.activeMessageIds = next;
    updateProcessingState();
  }
}

export function addPendingRequest(id: string) {
  if (!id) return;
  if (!messagesState.pendingRequests.has(id)) {
    const next = new Set(messagesState.pendingRequests);
    next.add(id);
    messagesState.pendingRequests = next;
    updateProcessingState();
  }
}

export function clearPendingRequest(id: string) {
  if (!id) return;
  if (messagesState.pendingRequests.has(id)) {
    const next = new Set(messagesState.pendingRequests);
    next.delete(id);
    messagesState.pendingRequests = next;
    updateProcessingState();
  }
}

export function clearProcessingState() {
  messagesState.backendProcessing = false;
  messagesState.activeMessageIds = new Set();
  messagesState.pendingRequests = new Set();
  updateProcessingState();
}

/** 获取后端处理状态（用于时序判断） */
export function getBackendProcessing(): boolean {
  return backendProcessing;
}

export function clearPendingInteractions() {
  pendingConfirmation = null;
  pendingRecovery = null;
  pendingQuestion = null;
  pendingClarification = null;
  pendingWorkerQuestion = null;
  pendingToolAuthorization = null;
}

export function addToast(type: string, message: string, title?: string) {
  const toast = {
    id: `toast_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
    type,
    title,
    message,
  };
  toasts = [...toasts, toast];
}

export function getActiveInteractionType(): string | null {
  if (pendingRecovery) return 'recovery';
  if (pendingConfirmation) return 'confirmation';
  if (pendingToolAuthorization) return 'toolAuthorization';
  if (pendingClarification) return 'clarification';
  if (pendingQuestion) return 'question';
  if (pendingWorkerQuestion) return 'workerQuestion';
  return null;
}
// 消息操作
export function addThreadMessage(message: Message) {
  // 完全重建数组以确保响应式更新
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;
  console.log('[MessagesStore] addThreadMessage 开始:', {
    id: safeMessage.id,
    role: safeMessage.role,
    type: safeMessage.type,
    currentCount: messagesState.threadMessages.length,
  });
  if (messagesState.threadMessages.some((m) => m.id === safeMessage.id)) {
    console.warn(`[MessagesStore] 跳过重复消息: ${safeMessage.id}`);
    // 🔧 改为警告而非抛出错误，避免中断
    return;
  }
  // 🔧 修复：使用对象属性赋值，确保响应式追踪
  messagesState.threadMessages = [...messagesState.threadMessages, safeMessage];
  console.log('[MessagesStore] addThreadMessage 完成:', {
    id: safeMessage.id,
    newCount: messagesState.threadMessages.length,
    allIds: messagesState.threadMessages.map(m => m.id),
  });
  saveWebviewState();
}

export function updateThreadMessage(messageId: string, updates: Partial<Message>) {
  const index = messagesState.threadMessages.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    // 必须完全重建数组，不能直接修改索引
    // 使用 JSON 序列化确保脱离响应式代理
    const normalizedUpdates: Partial<Message> = { ...updates };
    if ('blocks' in normalizedUpdates) {
      const blocks = sanitizeMessageBlocks(normalizedUpdates.blocks);
      normalizedUpdates.blocks = blocks.length > 0 ? blocks : undefined;
    }
    const safeUpdates = JSON.parse(JSON.stringify(normalizedUpdates)) as Partial<Message>;
    const newMessages = messagesState.threadMessages.map((msg, i) => {
      if (i === index) {
        return { ...msg, ...safeUpdates };
      }
      return msg;
    });
    messagesState.threadMessages = newMessages;
    // 不触发保存，由流式管理器批量保存
  }
}

export function replaceThreadMessage(oldMessageId: string, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;
  console.log('[MessagesStore] replaceThreadMessage 开始:', {
    oldMessageId,
    newMessageId: safeMessage.id,
    isPlaceholder: safeMessage.metadata?.isPlaceholder,
    wasPlaceholder: safeMessage.metadata?.wasPlaceholder,
    requestId: safeMessage.metadata?.requestId,
  });
  const index = messagesState.threadMessages.findIndex((m) => m.id === oldMessageId);
  if (index === -1) {
    console.log('[MessagesStore] replaceThreadMessage: 未找到旧消息，添加新消息');
    addThreadMessage(safeMessage);
    return;
  }
  if (messagesState.threadMessages.some((m, i) => m.id === safeMessage.id && i !== index)) {
    console.warn(`[MessagesStore] 替换消息 id 冲突: ${safeMessage.id}，跳过替换`);
    return;
  }
  const next = [...messagesState.threadMessages];
  next[index] = safeMessage;
  messagesState.threadMessages = next;
  console.log('[MessagesStore] replaceThreadMessage 完成:', {
    oldMessageId,
    newMessageId: safeMessage.id,
    totalMessages: messagesState.threadMessages.length,
  });
  saveWebviewState();
}

export function removeThreadMessage(messageId: string) {
  if (!messagesState.threadMessages.length) return;
  messagesState.threadMessages = messagesState.threadMessages.filter((m) => m.id !== messageId);
  saveWebviewState();
}

export function clearThreadMessages() {
  messagesState.threadMessages = [];
  saveWebviewState();
}

export function addAgentMessage(agent: AgentType, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;
  if (messagesState.agentOutputs[agent].some((m) => m.id === safeMessage.id)) {
    console.warn(`[MessagesStore] 跳过重复的 agent message: ${safeMessage.id}`);
    return;
  }

  // 🔧 调试日志：追踪 Worker 消息添加
  console.log('[DEBUG] addAgentMessage:', {
    agent,
    messageId: safeMessage.id,
    contentPreview: safeMessage.content?.substring(0, 100),
    currentCount: messagesState.agentOutputs[agent]?.length || 0,
  });

  messagesState.agentOutputs = {
    ...messagesState.agentOutputs,
    [agent]: [...messagesState.agentOutputs[agent], safeMessage],
  };

  console.log('[DEBUG] addAgentMessage 完成:', {
    agent,
    newCount: messagesState.agentOutputs[agent]?.length || 0,
  });

  saveWebviewState();
}

export function updateAgentMessage(agent: AgentType, messageId: string, updates: Partial<Message>) {
  const list = messagesState.agentOutputs[agent];
  const index = list.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    const normalizedUpdates: Partial<Message> = { ...updates };
    if ('blocks' in normalizedUpdates) {
      const blocks = sanitizeMessageBlocks(normalizedUpdates.blocks);
      normalizedUpdates.blocks = blocks.length > 0 ? blocks : undefined;
    }
    const safeUpdates = JSON.parse(JSON.stringify(normalizedUpdates)) as Partial<Message>;
    const next = list.map((msg, i) => (i === index ? { ...msg, ...safeUpdates } : msg));
    messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: next };
    // 不触发保存，由流式管理器批量保存
  }
}

export function replaceAgentMessage(agent: AgentType, oldMessageId: string, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;
  const index = messagesState.agentOutputs[agent].findIndex((m) => m.id === oldMessageId);
  if (index === -1) {
    addAgentMessage(agent, safeMessage);
    return;
  }
  if (messagesState.agentOutputs[agent].some((m, i) => m.id === safeMessage.id && i !== index)) {
    console.warn(`[MessagesStore] 替换 agent message id 冲突: ${safeMessage.id}`);
    return;
  }
  const next = [...messagesState.agentOutputs[agent]];
  next[index] = safeMessage;
  messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: next };
  saveWebviewState();
}

export function removeAgentMessage(agent: AgentType, messageId: string) {
  const list = messagesState.agentOutputs[agent];
  if (!list.length) return;
  const next = list.filter((m) => m.id !== messageId);
  if (next.length === list.length) return;
  messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: next };
  saveWebviewState();
}

export function clearAgentMessages(agent: AgentType) {
  messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: [] };
  saveWebviewState();
}

export function clearAgentOutputs() {
  messagesState.agentOutputs = {
    claude: [],
    codex: [],
    gemini: [],
  };
  saveWebviewState();
}

// 清空所有消息（用于会话切换/新建）
export function clearAllMessages() {
  messagesState.threadMessages = [];
  messagesState.agentOutputs = {
    claude: [],
    codex: [],
    gemini: [],
  };
  clearPendingInteractions();
  clearProcessingState();
  saveWebviewState();
}

// 设置完整的消息列表（用于会话切换时加载历史）
export function setThreadMessages(messages: Message[]) {
  messagesState.threadMessages = normalizePersistedMessages(messages).map(m => JSON.parse(JSON.stringify(m)) as Message);
  saveWebviewState();
}

// 设置完整的 agent 消息列表（用于会话切换时加载历史）
export function setAgentOutputs(outputs: AgentOutputs) {
  messagesState.agentOutputs = {
    claude: normalizePersistedMessages(outputs.claude).map(m => JSON.parse(JSON.stringify(m)) as Message),
    codex: normalizePersistedMessages(outputs.codex).map(m => JSON.parse(JSON.stringify(m)) as Message),
    gemini: normalizePersistedMessages(outputs.gemini).map(m => JSON.parse(JSON.stringify(m)) as Message),
  };
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
      throw new Error('[MessagesStore] 持久化数据结构无效');
    }
    // Tab 状态不持久化，每次打开都默认显示主对话 tab
    messagesState.currentTopTab = 'thread';
    messagesState.currentBottomTab = 'thread';
    messagesState.threadMessages = normalizePersistedMessages(persisted.threadMessages);
    messagesState.agentOutputs = {
      claude: normalizePersistedMessages(persisted.agentOutputs?.claude),
      codex: normalizePersistedMessages(persisted.agentOutputs?.codex),
      gemini: normalizePersistedMessages(persisted.agentOutputs?.gemini),
    };
    if (
      hasInvalidMessageSource(messagesState.threadMessages) ||
      hasInvalidMessageSource(messagesState.agentOutputs.claude) ||
      hasInvalidMessageSource(messagesState.agentOutputs.codex) ||
      hasInvalidMessageSource(messagesState.agentOutputs.gemini)
    ) {
      throw new Error('[MessagesStore] 持久化消息来源无效');
    }
    const sessionSeen = new Set<string>();
    messagesState.sessions = ensureArray<Session>(persisted.sessions)
      .filter((session) => !!session && typeof session.id === 'string' && session.id.trim().length > 0)
      .filter((session) => {
        if (sessionSeen.has(session.id)) return false;
        sessionSeen.add(session.id);
        return true;
      });
    messagesState.currentSessionId = persisted.currentSessionId || null;
    messagesState.scrollPositions = persisted.scrollPositions || { thread: 0, claude: 0, codex: 0, gemini: 0 };
    messagesState.autoScrollEnabled = persisted.autoScrollEnabled || { thread: true, claude: true, codex: true, gemini: true };
  }
}

// ============ Wave 状态操作（提案 4.6） ============

export function setWaveState(state: WaveState | null) {
  waveState = state;
}

export function updateWaveProgress(waveIndex: number, status: WaveState['status']) {
  if (waveState) {
    waveState = {
      ...waveState,
      currentWave: waveIndex,
      status,
    };
  }
}

export function clearWaveState() {
  waveState = null;
}

// ============ Worker Session 状态操作（提案 4.1） ============

export function addWorkerSession(session: WorkerSessionState) {
  const newSessions = new Map(workerSessions);
  newSessions.set(session.sessionId, session);
  workerSessions = newSessions;
}

export function updateWorkerSession(sessionId: string, updates: Partial<WorkerSessionState>) {
  const existing = workerSessions.get(sessionId);
  if (existing) {
    const newSessions = new Map(workerSessions);
    newSessions.set(sessionId, { ...existing, ...updates });
    workerSessions = newSessions;
  }
}

export function removeWorkerSession(sessionId: string) {
  const newSessions = new Map(workerSessions);
  newSessions.delete(sessionId);
  workerSessions = newSessions;
}

export function clearWorkerSessions() {
  workerSessions = new Map();
}

// ============ 请求-响应绑定操作（消息响应流设计） ============

/**
 * 创建请求绑定
 */
export function createRequestBinding(binding: RequestResponseBinding): void {
  const next = new Map(requestBindings);
  next.set(binding.requestId, binding);
  requestBindings = next;
}

/**
 * 获取请求绑定
 */
export function getRequestBinding(requestId: string): RequestResponseBinding | undefined {
  return requestBindings.get(requestId);
}

/**
 * 更新请求绑定（添加 realMessageId）
 */
export function updateRequestBinding(
  requestId: string,
  updates: Partial<RequestResponseBinding>
): void {
  const existing = requestBindings.get(requestId);
  if (existing) {
    const updated = { ...existing, ...updates };
    const next = new Map(requestBindings);
    next.set(requestId, updated);
    requestBindings = next;
  }
}

/**
 * 清除请求绑定
 */
export function clearRequestBinding(requestId: string): void {
  const next = new Map(requestBindings);
  next.delete(requestId);
  requestBindings = next;
}

/**
 * 根据占位消息 ID 查找请求绑定
 */
export function findBindingByPlaceholder(placeholderMessageId: string): RequestResponseBinding | undefined {
  for (const binding of requestBindings.values()) {
    if (binding.placeholderMessageId === placeholderMessageId) {
      return binding;
    }
  }
  return undefined;
}

/**
 * 清除所有请求绑定（会话切换时使用）
 */
export function clearAllRequestBindings(): void {
  requestBindings = new Map();
}

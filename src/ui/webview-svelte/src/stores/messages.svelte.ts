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
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray, generateId } from '../lib/utils';

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
let backendProcessing = $state(false);
let activeMessageIds = $state<Set<string>>(new Set());
let pendingRequests = $state<Set<string>>(new Set());
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
let currentPhase = $state(0);
let toasts = $state<Array<{ id: string; type: string; title?: string; message: string }>>([]);
let modelStatus = $state<Record<string, string>>({
  claude: 'unavailable',
  codex: 'unavailable',
  gemini: 'unavailable',
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
  const seen = new Set<string>();
  sessions = ensureArray<Session>(newSessions)
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
  backendProcessing = value;
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
  isProcessing = backendProcessing || activeMessageIds.size > 0 || pendingRequests.size > 0;
}

export function markMessageActive(id: string) {
  if (!id) return;
  if (!activeMessageIds.has(id)) {
    const next = new Set(activeMessageIds);
    next.add(id);
    activeMessageIds = next;
    updateProcessingState();
  }
}

export function markMessageComplete(id: string) {
  if (!id) return;
  if (activeMessageIds.has(id)) {
    const next = new Set(activeMessageIds);
    next.delete(id);
    activeMessageIds = next;
    updateProcessingState();
  }
}

export function addPendingRequest(id: string) {
  if (!id) return;
  if (!pendingRequests.has(id)) {
    const next = new Set(pendingRequests);
    next.add(id);
    pendingRequests = next;
    updateProcessingState();
  }
}

export function clearPendingRequest(id: string) {
  if (!id) return;
  if (pendingRequests.has(id)) {
    const next = new Set(pendingRequests);
    next.delete(id);
    pendingRequests = next;
    updateProcessingState();
  }
}

export function clearProcessingState() {
  backendProcessing = false;
  activeMessageIds = new Set();
  pendingRequests = new Set();
  updateProcessingState();
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
  if (threadMessages.some((m) => m.id === safeMessage.id)) {
    throw new Error(`[MessagesStore] 重复的 thread message id: ${safeMessage.id}`);
  }
  threadMessages = [...threadMessages, safeMessage];
  saveWebviewState();
}

export function updateThreadMessage(messageId: string, updates: Partial<Message>) {
  const index = threadMessages.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    // 必须完全重建数组，不能直接修改索引
    // 使用 JSON 序列化确保脱离响应式代理
    const normalizedUpdates: Partial<Message> = { ...updates };
    if ('blocks' in normalizedUpdates) {
      const blocks = sanitizeMessageBlocks(normalizedUpdates.blocks);
      normalizedUpdates.blocks = blocks.length > 0 ? blocks : undefined;
    }
    const safeUpdates = JSON.parse(JSON.stringify(normalizedUpdates)) as Partial<Message>;
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

export function removeThreadMessage(messageId: string) {
  if (!threadMessages.length) return;
  threadMessages = threadMessages.filter((m) => m.id !== messageId);
  saveWebviewState();
}

export function clearThreadMessages() {
  threadMessages = [];
  saveWebviewState();
}

export function addAgentMessage(agent: AgentType, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;
  if (agentOutputs[agent].some((m) => m.id === safeMessage.id)) {
    throw new Error(`[MessagesStore] 重复的 agent message id: ${safeMessage.id}`);
  }

  // 🔧 调试日志：追踪 Worker 消息添加
  console.log('[DEBUG] addAgentMessage:', {
    agent,
    messageId: safeMessage.id,
    contentPreview: safeMessage.content?.substring(0, 100),
    currentCount: agentOutputs[agent]?.length || 0,
  });

  agentOutputs = {
    ...agentOutputs,
    [agent]: [...agentOutputs[agent], safeMessage],
  };

  console.log('[DEBUG] addAgentMessage 完成:', {
    agent,
    newCount: agentOutputs[agent]?.length || 0,
  });

  saveWebviewState();
}

export function updateAgentMessage(agent: AgentType, messageId: string, updates: Partial<Message>) {
  const list = agentOutputs[agent];
  const index = list.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    const normalizedUpdates: Partial<Message> = { ...updates };
    if ('blocks' in normalizedUpdates) {
      const blocks = sanitizeMessageBlocks(normalizedUpdates.blocks);
      normalizedUpdates.blocks = blocks.length > 0 ? blocks : undefined;
    }
    const safeUpdates = JSON.parse(JSON.stringify(normalizedUpdates)) as Partial<Message>;
    const next = list.map((msg, i) => (i === index ? { ...msg, ...safeUpdates } : msg));
    agentOutputs = { ...agentOutputs, [agent]: next };
    // 不触发保存，由流式管理器批量保存
  }
}

export function removeAgentMessage(agent: AgentType, messageId: string) {
  const list = agentOutputs[agent];
  if (!list.length) return;
  const next = list.filter((m) => m.id !== messageId);
  if (next.length === list.length) return;
  agentOutputs = { ...agentOutputs, [agent]: next };
  saveWebviewState();
}

export function clearAgentMessages(agent: AgentType) {
  agentOutputs = { ...agentOutputs, [agent]: [] };
  saveWebviewState();
}

export function clearAgentOutputs() {
  agentOutputs = {
    claude: [],
    codex: [],
    gemini: [],
  };
  saveWebviewState();
}

// 清空所有消息（用于会话切换/新建）
export function clearAllMessages() {
  threadMessages = [];
  agentOutputs = {
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
  threadMessages = normalizePersistedMessages(messages).map(m => JSON.parse(JSON.stringify(m)) as Message);
  saveWebviewState();
}

// 设置完整的 agent 消息列表（用于会话切换时加载历史）
export function setAgentOutputs(outputs: AgentOutputs) {
  agentOutputs = {
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
    currentTopTab = 'thread';
    currentBottomTab = 'thread';
    threadMessages = normalizePersistedMessages(persisted.threadMessages);
    agentOutputs = {
      claude: normalizePersistedMessages(persisted.agentOutputs?.claude),
      codex: normalizePersistedMessages(persisted.agentOutputs?.codex),
      gemini: normalizePersistedMessages(persisted.agentOutputs?.gemini),
    };
    if (
      hasInvalidMessageSource(threadMessages) ||
      hasInvalidMessageSource(agentOutputs.claude) ||
      hasInvalidMessageSource(agentOutputs.codex) ||
      hasInvalidMessageSource(agentOutputs.gemini)
    ) {
      throw new Error('[MessagesStore] 持久化消息来源无效');
    }
    const sessionSeen = new Set<string>();
    sessions = ensureArray<Session>(persisted.sessions)
      .filter((session) => !!session && typeof session.id === 'string' && session.id.trim().length > 0)
      .filter((session) => {
        if (sessionSeen.has(session.id)) return false;
        sessionSeen.add(session.id);
        return true;
      });
    currentSessionId = persisted.currentSessionId || null;
    scrollPositions = persisted.scrollPositions || { thread: 0, claude: 0, codex: 0, gemini: 0 };
    autoScrollEnabled = persisted.autoScrollEnabled || { thread: true, claude: true, codex: true, gemini: true };
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

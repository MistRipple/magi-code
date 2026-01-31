/**
 * 消息处理器 - 处理来自 VS Code 扩展的消息
 */

import { vscode, type WebviewMessage } from '../lib/vscode-bridge';
import {
  getState,
  addThreadMessage,
  updateThreadMessage,
  addAgentMessage,
  updateAgentMessage,
  removeAgentMessage,
  setIsProcessing,
  setCurrentSessionId,
  updateSessions,
  setAppState,
  setMissionPlan,
  clearAllMessages,
  setThreadMessages,
  setAgentOutputs,
  removeThreadMessage,
  setWaveState,
  updateWaveProgress,
  addWorkerSession,
  updateWorkerSession,
} from '../stores/messages.svelte';
import type { Message, AppState, Session, ContentBlock, ToolCall, ThinkingBlock, MissionPlan, AssignmentPlan, AssignmentTodo, WaveState, WorkerSessionState, Task, Edit } from '../types/message';
import type { StandardMessage, StreamUpdate, ContentBlock as StandardContentBlock } from '../../../../protocol/message-protocol';
import { MessageType } from '../../../../protocol/message-protocol';
import { routeStandardMessage, getMessageTarget, clearMessageTargets } from './message-router';
import { normalizeWorkerSlot } from './message-classifier';
import { ensureArray } from './utils';
import { resolvePhaseStep } from '../config/phase-map';

// 生成唯一 ID
function generateId(): string {
  return `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}

function normalizeRestoredMessages(messages: Message[]): Message[] {
  const seen = new Set<string>();
  const normalized: Message[] = [];
  for (const msg of ensureArray<Message>(messages)) {
    if (!msg || typeof msg !== 'object') continue;
    const rawId = typeof msg.id === 'string' ? msg.id.trim() : '';
    const id = rawId || generateId();
    if (seen.has(id)) continue;
    seen.add(id);
    normalized.push({ ...msg, id });
  }
  return normalized;
}

// 标准消息的兜底 ID 生成（用于缺失 id 的异常消息）
function buildFallbackMessageId(standard: StandardMessage): string {
  const traceId = standard.traceId || 'no-trace';
  const timestamp = standard.timestamp || standard.updatedAt || Date.now();
  const source = standard.source || 'unknown';
  const agent = standard.agent || 'unknown';
  const type = standard.type || 'unknown';
  return `msg_fallback_${traceId}_${timestamp}_${source}_${agent}_${type}`;
}

function normalizeStandardMessage(standard: StandardMessage): StandardMessage {
  if (standard.id && standard.id.trim()) {
    return standard;
  }
  const fallbackId = buildFallbackMessageId(standard);
  console.warn('[MessageHandler] 标准消息缺少 id，已使用回退 id', { fallbackId, standard });
  return { ...standard, id: fallbackId };
}


/**
 * 添加系统通知消息（居中显示的简洁通知）
 */
export function addSystemMessage(content: string, noticeType: 'info' | 'success' | 'warning' | 'error' = 'info') {
  const message: Message = {
    id: generateId(),
    role: 'system',
    source: 'system',
    content,
    timestamp: Date.now(),
    type: 'system-notice',
    noticeType,
    isStreaming: false,
    isComplete: true,
  };
  addThreadMessage(message);
}

/**
 * 初始化消息处理器
 */
export function initMessageHandler() {
  vscode.onMessage(handleMessage);
  console.log('[MessageHandler] 消息处理器已初始化');
}

/**
 * 处理来自扩展的消息
 */
function handleMessage(message: WebviewMessage) {
  const { type } = message;

  switch (type) {
    case 'stateUpdate':
      handleStateUpdate(message);
      break;

    case 'standardMessage':
      handleStandardMessage(message);
      break;

    case 'standardUpdate':
      handleStandardUpdate(message);
      break;

    case 'standardComplete':
      handleStandardComplete(message);
      break;

    case 'processingStateChanged':
      handleProcessingStateChange(message);
      break;

    case 'phaseChanged':
      handlePhaseChanged(message);
      break;

    case 'sessionsUpdated':
      handleSessionsUpdated(message);
      break;

    case 'sessionCreated':
    case 'sessionLoaded':
    case 'sessionSwitched':
      handleSessionChanged(message);
      break;

    case 'sessionSummaryLoaded':
      handleSessionSummaryLoaded(message);
      break;

    case 'sessionMessagesLoaded':
      handleSessionMessagesLoaded(message);
      break;

    case 'confirmationRequest':
      handleConfirmationRequest(message);
      break;

    case 'recoveryRequest':
      handleRecoveryRequest(message);
      break;

    case 'questionRequest':
      handleQuestionRequest(message);
      break;

    case 'clarificationRequest':
      handleClarificationRequest(message);
      break;

    case 'workerQuestionRequest':
      handleWorkerQuestionRequest(message);
      break;

    case 'toolAuthorizationRequest':
      handleToolAuthorizationRequest(message);
      break;

    case 'missionPlanned':
      handleMissionPlanned(message);
      break;

    case 'assignmentPlanned':
      handleAssignmentPlanned(message);
      break;

    case 'assignmentStarted':
      handleAssignmentStarted(message);
      break;

    case 'assignmentCompleted':
      handleAssignmentCompleted(message);
      break;

    case 'todoStarted':
      handleTodoStarted(message);
      break;

    case 'todoCompleted':
      handleTodoCompleted(message);
      break;

    case 'todoFailed':
      handleTodoFailed(message);
      break;

    case 'dynamicTodoAdded':
      handleDynamicTodoAdded(message);
      break;

    case 'todoApprovalRequested':
      handleTodoApprovalRequested(message);
      break;

    // ============ Wave 执行事件（提案 4.6） ============
    case 'waveExecutionStarted':
      handleWaveExecutionStarted(message);
      break;

    case 'waveStarted':
      handleWaveStarted(message);
      break;

    case 'waveCompleted':
      handleWaveCompleted(message);
      break;

    // ============ Worker Session 事件（提案 4.1） ============
    case 'workerSessionCreated':
      handleWorkerSessionCreated(message);
      break;

    case 'workerSessionResumed':
      handleWorkerSessionResumed(message);
      break;

    // ============ 系统通知类消息 ============
    case 'workerStatusUpdate':
      handleWorkerStatusUpdate(message);
      break;

    case 'workerStatusChanged':
      addSystemMessage((message.worker as string) + ' 状态已更新', 'info');
      break;

    case 'workerError':
      addSystemMessage((message.worker as string) + ': ' + (message.error as string), 'error');
      break;

    case 'error':
      addSystemMessage((message.message as string) || '发生错误', 'error');
      break;

    case 'interactionModeChanged':
      getState().interactionMode = (message.mode as string) === 'ask' ? 'ask' : 'auto';
      addSystemMessage('已切换到 ' + getModeDisplayName(message.mode as string) + ' 模式', 'info');
      break;

    // verificationResult 已移除：验证结果在最终总结中统一显示，避免重复

    case 'recoveryResult':
      addSystemMessage(message.message as string, message.success ? 'success' : 'error');
      break;

    case 'workerFallbackNotice':
      addSystemMessage(`${message.originalWorker} 降级到 ${message.fallbackWorker}: ${message.reason}`, 'warning');
      break;

    case 'missionExecutionFailed':
      addSystemMessage((message.error as string) || '任务执行失败', 'error');
      setIsProcessing(false);
      break;

    case 'missionFailed':
      addSystemMessage((message.error as string) || '任务失败', 'error');
      setIsProcessing(false);
      break;

    case 'toast':
      handleToast(message);
      break;

    default:
      // 其他未处理的消息类型，静默忽略或记录日志
      // console.log('[MessageHandler] 未知消息类型:', type, message);
      break;
  }
}

/**
 * 获取交互模式显示名称
 */
function getModeDisplayName(mode: string): string {
  const modeNames: Record<string, string> = {
    'ask': '对话',
    'agent': '智能体',
    'orchestrator': '智能编排',
    'plan': '规划',
    'code': '编码',
    'auto': '自动',
  };
  return modeNames[mode] || mode;
}

// ============ 消息处理函数 ============

function handleStateUpdate(message: WebviewMessage) {
  const state = message.state as AppState;
  if (!state) return;

  setAppState(state);

  if (state.sessions) {
    updateSessions(ensureArray(state.sessions) as Session[]);
  }

  if ((state as any).currentSessionId) {
    setCurrentSessionId((state as any).currentSessionId as string);
  }

  const store = getState();
  store.tasks = ensureArray(state.tasks)
    .filter((task): task is Task => !!task && typeof task === 'object' && typeof (task as Task).status === 'string')
    .map((task) => ({
      id: task.id || `task_${Date.now()}_${Math.random().toString(36).substr(2, 5)}`,
      name: task.name || task.prompt || '',
      description: task.description,
      status: task.status,
    }));
  store.edits = ensureArray(state.pendingChanges)
    .map((change) => ({
      filePath: change.filePath,
      type: change.type,
      additions: change.additions,
      deletions: change.deletions,
      contributors: change.contributors,
      workerId: change.workerId,
    }));
  if (typeof (state as any).orchestratorPhase === 'string') {
    store.currentPhase = mapPhaseToStep((state as any).orchestratorPhase);
  } else if (typeof (state as any).orchestratorPhase === 'number') {
    store.currentPhase = (state as any).orchestratorPhase;
  } else {
    store.currentPhase = 0;
  }

  if (Array.isArray((state as any).workerStatuses)) {
    const statusMap: Record<string, string> = {};
    for (const status of (state as any).workerStatuses) {
      if (!status?.worker) continue;
      statusMap[status.worker] = status.available ? 'connected' : 'unavailable';
    }
    store.modelStatus = { ...store.modelStatus, ...statusMap };
  }

  if (typeof (state as any).isRunning === 'boolean') {
    setIsProcessing(Boolean((state as any).isRunning));
  } else if (typeof state.isProcessing === 'boolean') {
    setIsProcessing(state.isProcessing);
  }

  if (typeof state.interactionMode === 'string') {
    const store = getState();
    store.interactionMode = state.interactionMode === 'ask' ? 'ask' : 'auto';
  }
}


function handleStandardMessage(message: WebviewMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) return;
  const standard = normalizeStandardMessage(rawStandard);
  const uiMessage = mapStandardMessage(standard);
  if (!uiMessage.isStreaming && !hasRenderableContent(uiMessage)) {
    return;
  }
  const target = routeStandardMessage(standard);
  if (target.location === 'none' || target.location === 'task') {
    return;
  }
    if (target.location === 'thread') {
      const existing = getState().threadMessages.find(m => m.id === uiMessage.id);
      if (existing) {
        updateThreadMessage(uiMessage.id, uiMessage);
      } else {
        addThreadMessage(uiMessage);
      }
    } else if (target.location === 'worker') {
      const existing = getState().agentOutputs[target.worker].find(m => m.id === uiMessage.id);
      if (existing) {
        updateAgentMessage(target.worker, uiMessage.id, uiMessage);
      } else {
        addAgentMessage(target.worker, uiMessage);
      }
    } else if (target.location === 'both') {
      const threadExisting = getState().threadMessages.find(m => m.id === uiMessage.id);
      if (threadExisting) {
        updateThreadMessage(uiMessage.id, uiMessage);
      } else {
        addThreadMessage(uiMessage);
      }
      const agentExisting = getState().agentOutputs[target.worker].find(m => m.id === uiMessage.id);
      if (agentExisting) {
        updateAgentMessage(target.worker, uiMessage.id, uiMessage);
      } else {
        addAgentMessage(target.worker, uiMessage);
      }
    }
}

function handleStandardUpdate(message: WebviewMessage) {
  const rawUpdate = message.update as StreamUpdate;
  if (!rawUpdate?.messageId) return;
  const update = rawUpdate.messageId.trim() ? rawUpdate : null;
  if (!update) {
    console.warn('[MessageHandler] 流式更新缺少 messageId，已丢弃', rawUpdate);
    return;
  }
  const location = getMessageTarget(update.messageId);
  if (!location) return;
  if (location.location === 'none' || location.location === 'task') {
    return;
  }
  if (location.location === 'thread') {
    const existing = getState().threadMessages.find(m => m.id === update.messageId);
    if (existing) {
      updateThreadMessage(update.messageId, applyStreamUpdate(existing, update));
    }
    return;
  }
  if (location.location === 'worker') {
    const existing = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (existing) {
      updateAgentMessage(location.worker, update.messageId, applyStreamUpdate(existing, update));
    }
    return;
  }
  if (location.location === 'both') {
    const threadExisting = getState().threadMessages.find(m => m.id === update.messageId);
    if (threadExisting) {
      updateThreadMessage(update.messageId, applyStreamUpdate(threadExisting, update));
    }
    const agentExisting = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (agentExisting) {
      updateAgentMessage(location.worker, update.messageId, applyStreamUpdate(agentExisting, update));
    }
  }
}

function handleStandardComplete(message: WebviewMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) return;
  const standard = normalizeStandardMessage(rawStandard);
  const location = getMessageTarget(standard.id);
  if (!location) return;
  if (location.location === 'none' || location.location === 'task') {
    return;
  }
  const uiMessage = mapStandardMessage(standard);
  if (!hasRenderableContent(uiMessage)) {
    if (location.location === 'thread') {
      removeThreadMessage(standard.id);
    } else if (location.location === 'worker') {
      removeAgentMessage(location.worker, standard.id);
    } else if (location.location === 'both') {
      removeThreadMessage(standard.id);
      removeAgentMessage(location.worker, standard.id);
    }
    return;
  }
  if (location.location === 'thread') {
    updateThreadMessage(standard.id, uiMessage);
  } else if (location.location === 'worker') {
    updateAgentMessage(location.worker, standard.id, uiMessage);
  } else if (location.location === 'both') {
    updateThreadMessage(standard.id, uiMessage);
    updateAgentMessage(location.worker, standard.id, uiMessage);
  }
}

function handleProcessingStateChange(message: WebviewMessage) {
  const state = (message.state as { isProcessing?: boolean }) || {};
  if (typeof state.isProcessing === 'boolean') {
    setIsProcessing(state.isProcessing);
  }
}

function handlePhaseChanged(message: WebviewMessage) {
  const store = getState();
  if (typeof message.phase === 'string') {
    store.currentPhase = mapPhaseToStep(message.phase);
  } else if (Number.isFinite(message.phase as number)) {
    store.currentPhase = message.phase as number;
  }
  if (typeof message.isRunning === 'boolean') {
    setIsProcessing(message.isRunning);
  }
}

function mapPhaseToStep(phase: string): number {
  return resolvePhaseStep(phase);
}

function handleSessionsUpdated(message: WebviewMessage) {
  const sessions = message.sessions as Session[];
  if (sessions) {
    updateSessions(ensureArray(sessions));
  }
}

function handleSessionChanged(message: WebviewMessage) {
  // 获取新的 sessionId
  const newSessionId = message.sessionId as string || (message.session as Session)?.id;

  if (newSessionId) {
    const store = getState();
    const currentId = store.currentSessionId;

    // 如果是不同的会话，清空当前消息
    if (currentId !== newSessionId) {
      clearAllMessages();
      clearMessageTargets();
    }

    setCurrentSessionId(newSessionId);
  }
}

function handleSessionSummaryLoaded(message: WebviewMessage) {
  // 切换会话时，后端发送会话摘要而非完整历史
  // 清空当前消息并显示会话摘要
  const sessionId = message.sessionId as string;
  const summary = message.summary as any;

  if (sessionId) {
    clearAllMessages();
    clearMessageTargets();
    setCurrentSessionId(sessionId);

    // 如果有会话摘要，创建一个系统消息显示摘要
    if (summary) {
      const summaryContent = [
        `**会话恢复: ${summary.title || '未命名会话'}**`,
        '',
        summary.objective ? `**目标:** ${summary.objective}` : '',
        summary.completedTasks?.length ? `**已完成任务:** ${summary.completedTasks.length} 个` : '',
        summary.inProgressTasks?.length ? `**进行中任务:** ${summary.inProgressTasks.length} 个` : '',
        summary.codeChanges?.length ? `**代码变更:** ${summary.codeChanges.length} 个文件` : '',
        summary.pendingIssues?.length ? `**待解决问题:** ${summary.pendingIssues.length} 个` : '',
        '',
        `_消息历史: ${summary.messageCount || 0} 条 | 最后更新: ${summary.lastUpdated ? new Date(summary.lastUpdated).toLocaleString() : '未知'}_`,
      ].filter(Boolean).join('\n');

      addThreadMessage({
        id: generateId(),
        role: 'system',
        content: summaryContent,
        source: 'system',
        timestamp: Date.now(),
        isStreaming: false,
        isComplete: true,
        blocks: [{
          type: 'text',
          content: summaryContent,
        }],
      });
    }
  }
}

function handleSessionMessagesLoaded(message: WebviewMessage) {
  // 切换会话时，后端发送完整的消息历史（包括主对话和 worker 消息）
  const sessionId = message.sessionId as string;
  const messages = message.messages as any[];
  const workerMessages = message.workerMessages as { claude?: any[]; codex?: any[]; gemini?: any[] } | undefined;

  if (sessionId) {
    // 先清空当前消息
    clearAllMessages();
    clearMessageTargets();
    setCurrentSessionId(sessionId);

    // 格式化消息的辅助函数
    const formatMessage = (m: any): Message => ({
      id: m.id || generateId(),
      role: m.role || 'assistant',
      content: m.content || '',
      source: m.source || 'orchestrator',
      timestamp: m.timestamp || Date.now(),
      isStreaming: false,
      isComplete: true,
      blocks: m.blocks || [{
        type: 'text' as const,
        content: m.content || '',
      }],
    });

    // 加载主对话消息
    if (messages && messages.length > 0) {
      const formattedMessages: Message[] = normalizeRestoredMessages(messages.map(formatMessage));
      setThreadMessages(formattedMessages);
    }

    // 加载 worker 消息
    if (workerMessages) {
      setAgentOutputs({
        claude: normalizeRestoredMessages((workerMessages.claude || []).map(formatMessage)),
        codex: normalizeRestoredMessages((workerMessages.codex || []).map(formatMessage)),
        gemini: normalizeRestoredMessages((workerMessages.gemini || []).map(formatMessage)),
      });
    }
  }
}

function handleToast(message: WebviewMessage) {
  const store = getState();
  const toast = {
    id: `toast_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
    type: (message.toastType as string) || 'info',
    title: message.title as string | undefined,
    message: (message.message as string) || '',
  };
  const currentToasts = ensureArray(store.toasts) as typeof toast[];
  store.toasts = [...currentToasts, toast];
}

function handleConfirmationRequest(message: WebviewMessage) {
  const store = getState();
  if (store.interactionMode === 'auto') {
    vscode.postMessage({ type: 'confirmPlan', confirmed: true });
    setIsProcessing(true);
    return;
  }
  store.pendingConfirmation = {
    plan: message.plan,
    formattedPlan: message.formattedPlan as string | undefined,
  };
  setIsProcessing(false);
}

function handleRecoveryRequest(message: WebviewMessage) {
  const store = getState();
  if (store.interactionMode === 'auto') {
    const canRetry = Boolean(message.canRetry);
    const canRollback = Boolean(message.canRollback);
    const decision: 'retry' | 'rollback' | 'continue' = canRetry
      ? 'retry'
      : (canRollback ? 'rollback' : 'continue');
    vscode.postMessage({ type: 'confirmRecovery', decision });
    setIsProcessing(true);
    return;
  }
  store.pendingRecovery = {
    taskId: (message.taskId as string) || '',
    error: message.error,
    canRetry: Boolean(message.canRetry),
    canRollback: Boolean(message.canRollback),
  };
  setIsProcessing(false);
}

function handleQuestionRequest(message: WebviewMessage) {
  const store = getState();
  if (store.interactionMode === 'auto') {
    vscode.postMessage({ type: 'answerQuestions', answer: null });
    setIsProcessing(true);
    return;
  }
  store.pendingQuestion = {
    questions: ensureArray<string>(message.questions),
    plan: message.plan,
  };
  setIsProcessing(false);
}

function handleClarificationRequest(message: WebviewMessage) {
  const store = getState();
  if (store.interactionMode === 'auto') {
    vscode.postMessage({
      type: 'answerClarification',
      answers: null,
      additionalInfo: null,
    });
    setIsProcessing(true);
    return;
  }
  store.pendingClarification = {
    questions: ensureArray<string>(message.questions),
    context: message.context as string | undefined,
    ambiguityScore: message.ambiguityScore as number | undefined,
    originalPrompt: message.originalPrompt as string | undefined,
  };
  setIsProcessing(false);
}

function handleWorkerQuestionRequest(message: WebviewMessage) {
  const store = getState();
  if (store.interactionMode === 'auto') {
    vscode.postMessage({ type: 'answerWorkerQuestion', answer: null });
    setIsProcessing(true);
    return;
  }
  store.pendingWorkerQuestion = {
    workerId: (message.workerId as string) || '',
    question: (message.question as string) || '',
    context: message.context as string | undefined,
    options: message.options,
  };
  setIsProcessing(false);
}

function handleToolAuthorizationRequest(message: WebviewMessage) {
  const store = getState();
  if (store.interactionMode === 'auto') {
    vscode.postMessage({ type: 'toolAuthorizationResponse', allowed: true });
    setIsProcessing(true);
    return;
  }
  store.pendingToolAuthorization = {
    toolName: (message.toolName as string) || '',
    toolArgs: message.toolArgs,
  };
  setIsProcessing(false);
}

function handleMissionPlanned(message: WebviewMessage) {
  const missionId = (message.missionId as string) || '';
  const assignments = ensureArray(message.assignments) as any[];
  const mappedAssignments: AssignmentPlan[] = assignments.map((assignment) => ({
    id: assignment.id,
    workerId: assignment.workerId,
    responsibility: assignment.responsibility,
    status: assignment.status,
    progress: assignment.progress,
    todos: ensureArray(assignment.todos).map((todo: any) => ({
      id: todo.id,
      assignmentId: assignment.id,
      content: todo.content || '',
      reasoning: todo.reasoning,
      expectedOutput: todo.expectedOutput,
      type: todo.type || 'implementation',
      priority: typeof todo.priority === 'number' ? todo.priority : 3,
      status: todo.status || 'pending',
      outOfScope: Boolean(todo.outOfScope),
      approvalStatus: todo.approvalStatus,
      approvalNote: todo.approvalNote,
    })),
  }));
  const plan: MissionPlan = { missionId, assignments: mappedAssignments };
  setMissionPlan(plan);
}

function handleAssignmentPlanned(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todos = ensureArray(message.todos).map((todo: any) => ({
    id: todo.id,
    assignmentId,
    content: todo.content || '',
    reasoning: todo.reasoning,
    expectedOutput: todo.expectedOutput,
    type: todo.type || 'implementation',
    priority: typeof todo.priority === 'number' ? todo.priority : 3,
    status: todo.status || 'pending',
    outOfScope: Boolean(todo.outOfScope),
    approvalStatus: todo.approvalStatus,
    approvalNote: todo.approvalNote,
  }));

  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    todos,
  }));
}

function handleAssignmentStarted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    status: 'running',
  }));
}

function handleAssignmentCompleted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const success = Boolean(message.success);
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    status: success ? 'completed' : 'failed',
    progress: success ? 100 : assignment.progress,
  }));
}

function handleTodoStarted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'in_progress',
  }));
}

function handleTodoCompleted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'completed',
  }));
}

function handleTodoFailed(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'failed',
  }));
}

function handleDynamicTodoAdded(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todo = message.todo as any;
  const newTodo: AssignmentTodo = {
    id: todo?.id || `todo_${Date.now()}`,
    assignmentId,
    content: todo?.content || '',
    reasoning: todo?.reasoning,
    expectedOutput: todo?.expectedOutput,
    type: todo?.type || 'implementation',
    priority: typeof todo?.priority === 'number' ? todo.priority : 3,
    status: todo?.status || 'pending',
    outOfScope: Boolean(todo?.outOfScope),
    approvalStatus: todo?.approvalStatus,
    approvalNote: todo?.approvalNote,
  };
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    todos: [...assignment.todos, newTodo],
  }));
}

function handleTodoApprovalRequested(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  const reason = message.reason as string;
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    approvalStatus: 'pending',
    approvalNote: reason,
  }));
}

function mapStandardMessage(standard: StandardMessage): Message {
  const blocks = mapStandardBlocks(standard.blocks || []);
  const fallbackContent = standard.interaction?.prompt || '';
  const content = blocksToContent(blocks) || fallbackContent;
  const isStreaming = standard.lifecycle === 'streaming' || standard.lifecycle === 'started';
  const isComplete = standard.lifecycle === 'completed';
  const isSystemNotice = standard.type === MessageType.SYSTEM || standard.type === MessageType.ERROR;
  const isErrorNotice = standard.type === MessageType.ERROR;

  // 🔧 修复：明确区分消息来源与展示来源
  // - 标准消息的 source 只可能是 orchestrator/worker
  // - UI 需要展示具体 Worker 槽位（claude/codex/gemini）
  // - 只有 worker 消息才显示 Worker 徽章
  const originSource = standard.source;
  const agentSlot = normalizeWorkerSlot(standard.agent);
  const metaSlot = normalizeWorkerSlot((standard.metadata as { worker?: unknown } | undefined)?.worker);
  const resolvedWorker = agentSlot ?? metaSlot ?? null;
  const displaySource: Message['source'] =
    originSource === 'orchestrator'
      ? 'orchestrator'
      : (resolvedWorker ?? 'orchestrator');

  const baseMetadata = { ...(standard.metadata || {}) } as Record<string, unknown>;
  if (originSource !== 'worker' && baseMetadata.subTaskCard) {
    delete baseMetadata.subTaskCard;
  }

  const dispatchToWorker = Boolean(baseMetadata.dispatchToWorker);

  return {
    id: standard.id,
    role: isSystemNotice ? 'system' : 'assistant',
    source: displaySource,
    content,
    blocks,
    timestamp: standard.timestamp || Date.now(),
    isStreaming,
    isComplete,
    type: isSystemNotice ? 'system-notice' : 'message',
    noticeType: isSystemNotice ? (isErrorNotice ? 'error' : 'info') : undefined,
    metadata: {
      ...baseMetadata,
      interaction: standard.interaction,
      worker: originSource === 'worker'
        ? (resolvedWorker ?? undefined)
        : (dispatchToWorker ? (resolvedWorker ?? undefined) : undefined),
    },
  };
}

function hasRenderableContent(message: Message): boolean {
  if (message.type === 'system-notice') return true;
  if (message.metadata?.subTaskCard) return true;
  if (message.content && message.content.trim()) return true;
  if (message.blocks && message.blocks.length > 0) {
    return message.blocks.some((block) => {
      if (block.type === 'text' || block.type === 'code' || block.type === 'thinking') {
        return Boolean(block.content && block.content.trim());
      }
      if (block.type === 'tool_call') {
        return true;
      }
      if (block.type === 'file_change' || block.type === 'plan') {
        return true;
      }
      return false;
    });
  }
  return false;
}

function updateAssignmentPlan(assignmentId: string, updater: (assignment: AssignmentPlan) => AssignmentPlan) {
  const store = getState();
  const plan = store.missionPlan;
  if (!plan) return;
  const index = plan.assignments.findIndex((a) => a.id === assignmentId);
  if (index === -1) return;
  const nextAssignments = plan.assignments.map((assignment, i) =>
    i === index ? updater(assignment) : assignment
  );
  setMissionPlan({ ...plan, assignments: nextAssignments });
}

function updateTodo(
  assignmentId: string,
  todoId: string,
  updater: (todo: AssignmentTodo) => AssignmentTodo
) {
  updateAssignmentPlan(assignmentId, (assignment) => {
    const idx = assignment.todos.findIndex((todo) => todo.id === todoId);
    if (idx === -1) {
      const placeholder: AssignmentTodo = {
        id: todoId,
        assignmentId,
        content: '',
        type: 'implementation',
        priority: 3,
        status: 'pending',
      };
      return { ...assignment, todos: [...assignment.todos, updater(placeholder)] };
    }
    const nextTodos = assignment.todos.map((todo, i) => (i === idx ? updater(todo) : todo));
    return { ...assignment, todos: nextTodos };
  });
}

function mapStandardBlocks(blocks: StandardContentBlock[]): ContentBlock[] {
  return ensureArray<StandardContentBlock>(blocks).map((block) => {
    switch (block.type) {
      case 'code':
        return {
          type: 'code',
          content: block.content,
          language: block.language,
        };
      case 'thinking': {
        const thinking: ThinkingBlock = {
          content: block.content || '',
          isComplete: true,
        };
        return {
          type: 'thinking',
          content: block.content || '',
          thinking,
        };
      }
      case 'tool_call': {
        const toolCall: ToolCall = {
          id: block.toolId,
          name: block.toolName,
          arguments: safeParseJson(block.input) || {},
          status: mapToolStatus(block.status),
          result: block.output,
          error: block.error,
        };
        return {
          type: 'tool_call',
          content: '',
          toolCall,
        };
      }
      case 'file_change': {
        return {
          type: 'file_change',
          content: '',
          fileChange: {
            filePath: block.filePath,
            changeType: block.changeType,
            additions: block.additions,
            deletions: block.deletions,
            diff: block.diff,
          },
        };
      }
      case 'plan': {
        return {
          type: 'plan',
          content: '',
          plan: {
            goal: block.goal,
            analysis: block.analysis,
            constraints: block.constraints,
            acceptanceCriteria: block.acceptanceCriteria,
            riskLevel: block.riskLevel,
            riskFactors: block.riskFactors,
            rawJson: block.rawJson,
          },
        };
      }
      default:
        return { type: 'text', content: block.content || '' };
    }
  });
}

function applyStreamUpdate(message: Message, update: StreamUpdate): Partial<Message> {
  const updates: Partial<Message> = {};
  if (update.updateType === 'append' && update.appendText) {
    updates.content = (message.content || '') + update.appendText;
    if (message.blocks && message.blocks.length > 0) {
      const nextBlocks = [...message.blocks];
      let lastTextIndex = -1;
      for (let i = nextBlocks.length - 1; i >= 0; i--) {
        if (nextBlocks[i].type === 'text') {
          lastTextIndex = i;
          break;
        }
      }
      if (lastTextIndex >= 0) {
        const current = nextBlocks[lastTextIndex];
        nextBlocks[lastTextIndex] = {
          ...current,
          content: (current.content || '') + update.appendText,
        };
      } else {
        nextBlocks.push({ type: 'text', content: update.appendText });
      }
        updates.blocks = nextBlocks;
      }
  } else if (update.updateType === 'replace') {
    if (update.blocks) {
      const blocks = mapStandardBlocks(update.blocks);
      updates.blocks = blocks;
      updates.content = blocksToContent(blocks);
    }
  } else if (update.updateType === 'block_update') {
    if (update.blocks) {
      const incoming = mapStandardBlocks(update.blocks);
      const merged = mergeBlocks(message.blocks || [], incoming);
      updates.blocks = merged;
      updates.content = blocksToContent(merged);
    }
  } else if (update.updateType === 'lifecycle_change' && update.lifecycle) {
    updates.isStreaming = update.lifecycle === 'streaming' || update.lifecycle === 'started';
    updates.isComplete = update.lifecycle === 'completed';
  }
  return updates;
}

function mergeBlocks(existing: ContentBlock[], incoming: ContentBlock[]): ContentBlock[] {
  const next = [...existing];
  for (const block of incoming) {
    if (block.type === 'tool_call' && block.toolCall?.id) {
      const idx = next.findIndex((b) => b.type === 'tool_call' && b.toolCall?.id === block.toolCall?.id);
      if (idx >= 0) {
        const prev = next[idx];
        next[idx] = {
          ...prev,
          ...block,
          toolCall: { ...prev.toolCall, ...block.toolCall },
        };
      } else {
        next.push(block);
      }
      continue;
    }
    if (block.type === 'thinking') {
      const idx = next.findIndex((b) => b.type === 'thinking');
      if (idx >= 0) {
        const prev = next[idx];
        // 🔧 修复：确保 content 字段始终有值
        const prevThinking = prev.thinking || { content: '', isComplete: false };
        const blockThinking = block.thinking || { content: '', isComplete: false };
        const mergedThinking = {
          content: blockThinking.content || prevThinking.content || block.content || prev.content || '',
          isComplete: blockThinking.isComplete ?? prevThinking.isComplete ?? true,
        };
        next[idx] = {
          ...prev,
          ...block,
          thinking: mergedThinking,
        };
      } else {
        next.push(block);
      }
      continue;
    }
    if (block.type === 'text') {
      const idx = [...next].map((b) => b.type).lastIndexOf('text');
      if (idx >= 0) {
        const prev = next[idx];
        next[idx] = { ...prev, content: (prev.content || '') + (block.content || '') };
      } else {
        next.push(block);
      }
      continue;
    }
    next.push(block);
  }
  return next;
}

function blocksToContent(blocks: ContentBlock[]): string {
  const textParts: string[] = [];
  for (const block of blocks) {
    if (!block) continue;
    if (block.type === 'text' || block.type === 'code' || block.type === 'thinking') {
      if (block.content) textParts.push(block.content);
    }
    if (block.type === 'file_change' && block.fileChange) {
      textParts.push(`文件变更: ${block.fileChange.filePath} (${block.fileChange.changeType})`);
    }
    if (block.type === 'plan' && block.plan) {
      textParts.push(formatPlanBlock(block.plan));
    }
  }
  return textParts.join('\\n\\n');
}

function mapToolStatus(status: string | undefined): ToolCall['status'] {
  switch (status) {
    case 'pending':
      return 'pending';
    case 'running':
      return 'running';
    case 'completed':
      return 'success';
    case 'failed':
      return 'error';
    default:
      return 'success';
  }
}

function safeParseJson(value?: string): Record<string, unknown> | null {
  if (!value || typeof value !== 'string') return null;
  try {
    return JSON.parse(value) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function formatPlanBlock(block: any): string {
  const parts: string[] = [];
  if (block.goal) parts.push(`目标: ${block.goal}`);
  if (block.analysis) parts.push(`分析: ${block.analysis}`);
  if (Array.isArray(block.constraints) && block.constraints.length > 0) {
    parts.push(`约束:\\n- ${block.constraints.join('\\n- ')}`);
  }
  if (Array.isArray(block.acceptanceCriteria) && block.acceptanceCriteria.length > 0) {
    parts.push(`验收标准:\\n- ${block.acceptanceCriteria.join('\\n- ')}`);
  }
  if (block.riskLevel) parts.push(`风险等级: ${block.riskLevel}`);
  if (Array.isArray(block.riskFactors) && block.riskFactors.length > 0) {
    parts.push(`风险因素:\\n- ${block.riskFactors.join('\\n- ')}`);
  }
  return parts.join('\\n\\n');
}

/**
 * 处理 Worker 状态更新消息
 * 将检测到的模型状态同步到全局 store，供 BottomTabs 等组件使用
 */
function handleWorkerStatusUpdate(message: WebviewMessage) {
  const statuses = message.statuses as Record<string, { status: string; model?: string; error?: string }>;
  if (!statuses) return;

  const store = getState();
  const statusMap: Record<string, string> = {};

  // 将详细状态映射为简化状态（connected/unavailable）
  for (const [worker, detail] of Object.entries(statuses)) {
    if (detail.status === 'available') {
      statusMap[worker] = 'connected';
    } else if (detail.status === 'checking') {
      // 检测中保持原状态
      continue;
    } else {
      statusMap[worker] = 'unavailable';
    }
  }

  // 更新全局 modelStatus
  store.modelStatus = { ...store.modelStatus, ...statusMap };
}

// ============ Wave 执行事件处理（提案 4.6） ============

function handleWaveExecutionStarted(message: WebviewMessage) {
  const totalWaves = (message.totalWaves as number) || 0;
  const waves = (message.waves as string[][]) || [];
  const criticalPath = (message.criticalPath as string[]) || [];

  const state: WaveState = {
    currentWave: 0,
    totalWaves,
    waves,
    criticalPath,
    status: 'executing',
  };

  setWaveState(state);

  // 添加系统通知
  addSystemMessage(`开始 Wave 执行: ${totalWaves} 个 Wave`, 'info');
}

function handleWaveStarted(message: WebviewMessage) {
  const waveIndex = (message.waveIndex as number) || 0;
  const totalWaves = (message.totalWaves as number) || 0;

  updateWaveProgress(waveIndex, 'executing');

  // 添加系统通知
  addSystemMessage(`Wave ${waveIndex + 1}/${totalWaves} 开始执行`, 'info');
}

function handleWaveCompleted(message: WebviewMessage) {
  const waveIndex = (message.waveIndex as number) || 0;
  const totalWaves = (message.totalWaves as number) || 0;
  const completedCount = (message.completedCount as number) || 0;
  const failedCount = (message.failedCount as number) || 0;

  // 检查是否所有 Wave 都完成
  const isLastWave = waveIndex >= totalWaves - 1;

  if (isLastWave) {
    updateWaveProgress(waveIndex, 'completed');
  } else {
    updateWaveProgress(waveIndex + 1, 'executing');
  }

  // 添加系统通知
  const statusText = failedCount > 0
    ? `成功 ${completedCount}, 失败 ${failedCount}`
    : `成功 ${completedCount}`;
  addSystemMessage(`Wave ${waveIndex + 1}/${totalWaves} 完成: ${statusText}`, failedCount > 0 ? 'warning' : 'success');
}

// ============ Worker Session 事件处理（提案 4.1） ============

function handleWorkerSessionCreated(message: WebviewMessage) {
  const sessionId = (message.sessionId as string) || '';
  const assignmentId = (message.assignmentId as string) || '';
  const workerId = (message.workerId as string) || '';

  if (!sessionId) return;

  const session: WorkerSessionState = {
    sessionId,
    assignmentId,
    workerId,
    isResumed: false,
    completedTodos: 0,
  };

  addWorkerSession(session);
}

function handleWorkerSessionResumed(message: WebviewMessage) {
  const sessionId = (message.sessionId as string) || '';
  const assignmentId = (message.assignmentId as string) || '';
  const completedTodos = (message.completedTodos as number) || 0;

  if (!sessionId) return;

  // 更新现有 session 或创建新的
  const store = getState();
  const existing = store.workerSessions.get(sessionId);

  if (existing) {
    updateWorkerSession(sessionId, {
      isResumed: true,
      completedTodos,
    });
  } else {
    const session: WorkerSessionState = {
      sessionId,
      assignmentId,
      workerId: (message.workerId as string) || '',
      isResumed: true,
      completedTodos,
    };
    addWorkerSession(session);
  }

  // 添加系统通知
  addSystemMessage(`Session 已恢复，继续执行 ${completedTodos} 个已完成的 Todo`, 'info');
}

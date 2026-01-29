/**
 * 消息处理器 - 处理来自 VS Code 扩展的消息
 */

import { vscode, type WebviewMessage } from '../lib/vscode-bridge';
import {
  getState,
  addThreadMessage,
  updateThreadMessage,
  setIsProcessing,
  setCurrentSessionId,
  updateSessions,
  setAppState,
} from '../stores/messages.svelte';
import type { Message, AppState, Session, ContentBlock, ToolCall, ThinkingBlock } from '../types/message';
import type { StandardMessage, StreamUpdate, ContentBlock as StandardContentBlock } from '../../../../protocol/message-protocol';
import { ensureArray } from './utils';

// 生成唯一 ID
function generateId(): string {
  return `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
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

    // ============ 系统通知类消息 ============
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
      addSystemMessage('已切换到 ' + getModeDisplayName(message.mode as string) + ' 模式', 'info');
      break;

    case 'verificationResult':
      if (message.success) {
        addSystemMessage('验证通过: ' + (message.summary as string), 'success');
      } else {
        addSystemMessage('验证失败: ' + (message.summary as string), 'error');
      }
      break;

    case 'recoveryResult':
      addSystemMessage(message.message as string, message.success ? 'success' : 'error');
      break;

    case 'workerFallbackNotice':
      addSystemMessage(`${message.originalWorker} 降级到 ${message.fallbackWorker}: ${message.reason}`, 'warning');
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
  store.tasks = ensureArray(state.tasks);
  store.edits = ensureArray(state.pendingChanges);
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
}


function handleStandardMessage(message: WebviewMessage) {
  const standard = message.message as StandardMessage;
  if (!standard) return;
  const uiMessage = mapStandardMessage(standard);
  const state = getState();
  const existing = state.threadMessages.find(m => m.id === uiMessage.id);
  if (existing) {
    updateThreadMessage(uiMessage.id, uiMessage);
  } else {
    addThreadMessage(uiMessage);
  }
}

function handleStandardUpdate(message: WebviewMessage) {
  const update = message.update as StreamUpdate;
  if (!update?.messageId) return;
  const state = getState();
  const existing = state.threadMessages.find(m => m.id === update.messageId);
  if (!existing) return;
  updateThreadMessage(update.messageId, applyStreamUpdate(existing, update));
}

function handleStandardComplete(message: WebviewMessage) {
  const standard = message.message as StandardMessage;
  if (!standard) return;
  updateThreadMessage(standard.id, mapStandardMessage(standard));
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
  const normalized = phase.toLowerCase();
  switch (normalized) {
    case 'clarifying':
    case 'analyzing':
      return 1;
    case 'waiting_confirmation':
      return 2;
    case 'dispatching':
    case 'monitoring':
    case 'waiting_questions':
    case 'waiting_worker_answer':
      return 3;
    case 'integrating':
      return 4;
    case 'verifying':
      return 5;
    case 'recovering':
      return 6;
    case 'summarizing':
    case 'completed':
    case 'failed':
      return 7;
    case 'idle':
    default:
      return 0;
  }
}

function handleSessionsUpdated(message: WebviewMessage) {
  const sessions = message.sessions as Session[];
  if (sessions) {
    updateSessions(ensureArray(sessions));
  }
}

function handleSessionChanged(message: WebviewMessage) {
  if (message.sessionId) {
    setCurrentSessionId(message.sessionId as string);
  } else if (message.session && (message.session as Session).id) {
    setCurrentSessionId((message.session as Session).id);
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
  store.pendingConfirmation = {
    plan: message.plan,
    formattedPlan: message.formattedPlan as string | undefined,
  };
  setIsProcessing(false);
}

function handleRecoveryRequest(message: WebviewMessage) {
  const store = getState();
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
  store.pendingQuestion = {
    questions: ensureArray<string>(message.questions),
    plan: message.plan,
  };
  setIsProcessing(false);
}

function handleClarificationRequest(message: WebviewMessage) {
  const store = getState();
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
  store.pendingToolAuthorization = {
    toolName: (message.toolName as string) || '',
    toolArgs: message.toolArgs,
  };
  setIsProcessing(false);
}

function mapStandardMessage(standard: StandardMessage): Message {
  const blocks = mapStandardBlocks(standard.blocks || []);
  const content = blocksToContent(blocks);
  const isStreaming = standard.lifecycle === 'streaming' || standard.lifecycle === 'started';
  const isComplete = standard.lifecycle === 'completed';
  const isSystemNotice = standard.type === 'system-notice';

  // 🔧 修复：确保 source 始终有有效值
  // 优先级：agent > source > 默认值 'orchestrator'
  // 注：agent 字段是必需的，但旧消息或某些路径可能遗漏，需要容错
  const source = standard.agent || standard.source || 'orchestrator';

  return {
    id: standard.id,
    role: isSystemNotice ? 'system' : 'assistant',
    source: source as Message['source'],
    content,
    blocks,
    timestamp: standard.timestamp || Date.now(),
    isStreaming,
    isComplete,
    type: isSystemNotice ? 'system-notice' : 'message',
    noticeType: isSystemNotice ? 'info' : undefined,
    metadata: { ...standard.metadata, worker: standard.agent || standard.source },
  };
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
        const summary = `文件变更: ${block.filePath} (${block.changeType})`;
        return { type: 'text', content: summary };
      }
      case 'plan': {
        const formatted = formatPlanBlock(block);
        return { type: 'text', content: formatted };
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
  } else if (update.updateType === 'replace' || update.updateType === 'block_update') {
    if (update.blocks) {
      const blocks = mapStandardBlocks(update.blocks);
      updates.blocks = blocks;
      updates.content = blocksToContent(blocks);
    }
  } else if (update.updateType === 'lifecycle_change' && update.lifecycle) {
    updates.isStreaming = update.lifecycle === 'streaming' || update.lifecycle === 'started';
    updates.isComplete = update.lifecycle === 'completed';
  }
  return updates;
}

function blocksToContent(blocks: ContentBlock[]): string {
  const textParts: string[] = [];
  for (const block of blocks) {
    if (!block) continue;
    if (block.type === 'text' || block.type === 'code' || block.type === 'thinking') {
      if (block.content) textParts.push(block.content);
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

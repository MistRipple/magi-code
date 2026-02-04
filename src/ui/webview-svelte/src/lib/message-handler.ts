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
  replaceThreadMessage,
  removeThreadMessage,
  setIsProcessing,
  setCurrentSessionId,
  updateSessions,
  setAppState,
  setMissionPlan,
  clearAllMessages,
  setThreadMessages,
  setAgentOutputs,
  addToast,
  addWorkerSession,
  updateWorkerSession,
  markMessageActive,
  markMessageComplete,
  addPendingRequest,
  clearProcessingState,
  clearPendingRequest,
  setProcessingActor,
  getBackendProcessing,
  getRequestBinding,
  createRequestBinding,
  updateRequestBinding,
  clearRequestBinding,
  clearAllRequestBindings,
} from '../stores/messages.svelte';
import type { Message, AppState, Session, ContentBlock, ToolCall, ThinkingBlock, MissionPlan, AssignmentPlan, AssignmentTodo, WorkerSessionState, Task, Edit, ModelStatusMap } from '../types/message';
import type { StandardMessage, StreamUpdate, ContentBlock as StandardContentBlock } from '../../../../protocol/message-protocol';
import { MessageType, MessageCategory } from '../../../../protocol/message-protocol';
import { routeStandardMessage, getMessageTarget, clearMessageTargets, clearMessageTarget } from './message-router';
import { normalizeWorkerSlot } from './message-classifier';
import { ensureArray } from './utils';

function normalizeRestoredMessages(messages: Message[]): Message[] {
  const seen = new Set<string>();
  const normalized: Message[] = [];
  for (const msg of ensureArray<Message>(messages)) {
    if (!msg || typeof msg !== 'object') {
      throw new Error('[MessageHandler] 恢复消息包含无效对象');
    }
    const rawId = typeof msg.id === 'string' ? msg.id.trim() : '';
    if (!rawId) {
      throw new Error('[MessageHandler] 恢复消息缺少 id');
    }
    if (seen.has(rawId)) {
      throw new Error(`[MessageHandler] 恢复消息 id 重复: ${rawId}`);
    }
    seen.add(rawId);
    normalized.push({ ...msg, id: rawId });
  }
  return normalized;
}

function assertStandardMessageId(standard: StandardMessage): StandardMessage {
  if (standard.id && standard.id.trim()) {
    return standard;
  }
  throw new Error('[MessageHandler] 标准消息缺少 id');
}

function extractTextFromStandardBlocks(blocks?: StandardContentBlock[]): string {
  if (!Array.isArray(blocks) || blocks.length === 0) return '';
  return blocks
    .filter((block) => block.type === 'text' || block.type === 'thinking')
    .map((block) => (block as any).content || '')
    .filter(Boolean)
    .join('\n');
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

  // 🔧 调试日志：追踪所有收到的消息
  console.log('[MessageHandler] 收到消息:', {
    type,
    hasMessage: !!(message as any).message,
    messageId: (message as any).message?.id,
    category: (message as any).message?.category,
  });

  switch (type) {
    case 'unifiedMessage':
      handleUnifiedMessage(message);
      break;

    case 'unifiedUpdate':
      handleStandardUpdate(message);
      break;

    case 'unifiedComplete':
      handleStandardComplete(message);
      break;

    default:
      // 🔧 调试日志：追踪未处理的消息类型
      console.warn('[MessageHandler] 未知消息类型:', type, message);
      break;
  }
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
  const taskSeen = new Set<string>();
  store.tasks = ensureArray(state.tasks)
    .filter((task): task is Task => !!task && typeof task === 'object' && typeof (task as Task).status === 'string')
    .map((task) => {
      const id = typeof task.id === 'string' && task.id.trim() ? task.id.trim() : '';
      if (!id) {
        throw new Error('[MessageHandler] Task 缺少 id');
      }
      if (taskSeen.has(id)) {
        throw new Error(`[MessageHandler] Task id 重复: ${id}`);
      }
      taskSeen.add(id);
      return {
        id,
      name: task.name || task.prompt || '',
      description: task.description,
      status: task.status,
      };
    });
  const editSeen = new Set<string>();
  store.edits = ensureArray(state.pendingChanges)
    .filter((change): change is Edit => !!change && typeof change === 'object' && typeof (change as Edit).filePath === 'string' && !!(change as Edit).filePath)
    .filter((change) => {
      if (editSeen.has(change.filePath)) {
        return false;
      }
      editSeen.add(change.filePath);
      return true;
    })
    .map((change) => ({
      filePath: change.filePath,
      type: change.type,
      additions: change.additions,
      deletions: change.deletions,
      contributors: change.contributors,
      workerId: change.workerId,
    }));
  if (Array.isArray((state as any).workerStatuses)) {
    const statusMap: ModelStatusMap = {};
    for (const status of (state as any).workerStatuses) {
      if (!status?.worker) continue;
      const worker = status.worker;
      const currentStatus = store.modelStatus[worker]?.status;
      // 🔧 修复：只有当前状态为 'checking'（初始状态）时才使用 workerStatuses 更新
      // workerStatusUpdate 通过真实 LLM 连接测试得出，比 adapter.isConnected() 更准确
      // 避免 stateUpdate 中的 available: false 覆盖已经检测到的可用状态
      if (currentStatus === 'checking') {
        statusMap[worker] = {
          status: status.available ? 'available' : 'unavailable',
        };
      }
    }
    if (Object.keys(statusMap).length > 0) {
      store.modelStatus = { ...store.modelStatus, ...statusMap };
    }
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


function handleUnifiedMessage(message: WebviewMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) {
    console.error('[MessageHandler] unifiedMessage 缺少 message 字段:', message);
    throw new Error('[MessageHandler] unifiedMessage 缺少 message');
  }
  const standard = assertStandardMessageId(rawStandard);

  // 🔧 调试日志：追踪消息分类
  console.log('[MessageHandler] handleUnifiedMessage:', {
    id: standard.id,
    category: standard.category,
    expectedContentCategory: MessageCategory.CONTENT,
    isContentMatch: standard.category === MessageCategory.CONTENT,
    source: standard.source,
    agent: standard.agent,
    lifecycle: standard.lifecycle,
  });

  switch (standard.category) {
    case MessageCategory.CONTENT:
      console.log('[MessageHandler] 进入 handleContentMessage');
      handleContentMessage(standard);
      break;
    case MessageCategory.CONTROL:
      console.log('[MessageHandler] 进入 handleUnifiedControlMessage');
      handleUnifiedControlMessage(standard);
      break;
    case MessageCategory.NOTIFY:
      console.log('[MessageHandler] 进入 handleUnifiedNotify');
      handleUnifiedNotify(standard);
      break;
    case MessageCategory.DATA:
      console.log('[MessageHandler] 进入 handleUnifiedData');
      handleUnifiedData(standard);
      break;
    default:
      console.warn('[MessageHandler] 未知消息类别:', standard.category, standard);
      break;
  }
}

// ===== 流式更新缓冲：防止 update 先于 message 到达导致更新丢失 =====
const pendingStreamUpdates = new Map<string, StreamUpdate[]>();

function queueStreamUpdate(update: StreamUpdate): void {
  const list = pendingStreamUpdates.get(update.messageId) || [];
  list.push(update);
  pendingStreamUpdates.set(update.messageId, list);
  console.warn('[MessageHandler] 缓存流式更新', {
    messageId: update.messageId,
    updateType: update.updateType,
    cachedCount: list.length,
  });
}

function applyUpdateToLocation(location: ReturnType<typeof getMessageTarget>, update: StreamUpdate): boolean {
  if (!location) return false;
  if (location.location === 'none' || location.location === 'task') {
    return true;
  }

  let applied = false;
  if (location.location === 'thread') {
    const existing = getState().threadMessages.find(m => m.id === update.messageId);
    if (existing) {
      const streamUpdates = applyStreamUpdate(existing, update);
      let nextMessage: Message = { ...existing, ...streamUpdates };
      if (existing.metadata?.isPlaceholder && hasRenderableContent(nextMessage)) {
        nextMessage = {
          ...nextMessage,
          metadata: {
            ...(nextMessage.metadata || {}),
            isPlaceholder: false,
            wasPlaceholder: true,
            placeholderState: undefined,
          },
        };
      }
      updateThreadMessage(update.messageId, nextMessage);
      applied = true;
    }
  } else if (location.location === 'worker') {
    const existing = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (existing) {
      const streamUpdates = applyStreamUpdate(existing, update);
      updateAgentMessage(location.worker, update.messageId, { ...existing, ...streamUpdates });
      applied = true;
    }
  } else if (location.location === 'both') {
    const threadExisting = getState().threadMessages.find(m => m.id === update.messageId);
    if (threadExisting) {
      const streamUpdates = applyStreamUpdate(threadExisting, update);
      let nextMessage: Message = { ...threadExisting, ...streamUpdates };
      if (threadExisting.metadata?.isPlaceholder && hasRenderableContent(nextMessage)) {
        nextMessage = {
          ...nextMessage,
          metadata: {
            ...(nextMessage.metadata || {}),
            isPlaceholder: false,
            wasPlaceholder: true,
            placeholderState: undefined,
          },
        };
      }
      updateThreadMessage(update.messageId, nextMessage);
      applied = true;
    }
    const agentExisting = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (agentExisting) {
      const streamUpdates = applyStreamUpdate(agentExisting, update);
      updateAgentMessage(location.worker, update.messageId, { ...agentExisting, ...streamUpdates });
      applied = true;
    }
  }
  return applied;
}

function flushPendingStreamUpdates(messageId: string): void {
  const updates = pendingStreamUpdates.get(messageId);
  if (!updates || updates.length === 0) {
    return;
  }
  const location = getMessageTarget(messageId);
  if (!location) {
    return;
  }
  console.log('[MessageHandler] 回放缓存流式更新', {
    messageId,
    pendingCount: updates.length,
    location: location.location,
  });
  const remaining: StreamUpdate[] = [];
  for (const update of updates) {
    const applied = applyUpdateToLocation(location, update);
    if (!applied) {
      remaining.push(update);
    }
  }
  if (remaining.length > 0) {
    pendingStreamUpdates.set(messageId, remaining);
    console.warn('[MessageHandler] 流式更新仍未应用', {
      messageId,
      remainingCount: remaining.length,
    });
  } else {
    pendingStreamUpdates.delete(messageId);
    console.log('[MessageHandler] 流式更新回放完成', { messageId });
  }
}

function handleContentMessage(standard: StandardMessage) {
  // 🔧 调试日志：进入 handleContentMessage
  console.log('[MessageHandler] handleContentMessage 开始处理:', {
    id: standard.id,
    source: standard.source,
    agent: standard.agent,
    lifecycle: standard.lifecycle,
    blocksCount: standard.blocks?.length ?? 0,
    metadata: standard.metadata,
  });

  const uiMessage = mapStandardMessage(standard);
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const requestId = meta?.requestId as string | undefined;
  const isPlaceholder = Boolean(meta?.isPlaceholder);
  const isUserMessage = meta?.role === 'user';

  console.log('[MessageHandler] 消息类型判断:', {
    isPlaceholder,
    isUserMessage,
    requestId,
    isStreaming: uiMessage.isStreaming,
    hasRenderableContent: hasRenderableContent(uiMessage),
  });

  const upsertThreadMessage = (message: Message) => {
    console.log('[MessageHandler] upsertThreadMessage:', { id: message.id, type: message.type });
    const existing = getState().threadMessages.find(m => m.id === message.id);
    if (existing) {
      updateThreadMessage(message.id, message);
    } else {
      addThreadMessage(message);
    }
  };

  if (isPlaceholder) {
    console.log('[MessageHandler] 处理占位消息');
    if (!requestId) {
      throw new Error('[MessageHandler] 占位消息缺少 requestId');
    }
    const userMessageId = meta?.userMessageId as string | undefined;
    if (!userMessageId) {
      throw new Error('[MessageHandler] 占位消息缺少 userMessageId');
    }
    const binding = getRequestBinding(requestId);

    // 🔧 创建 60 秒超时定时器（首 token 超时保护）
    const timeoutId = setTimeout(() => {
      const currentBinding = getRequestBinding(requestId);
      // 只有在没有收到真实消息时才触发超时
      if (currentBinding && !currentBinding.realMessageId) {
        console.warn('[MessageHandler] 首 token 超时，移除占位消息:', requestId);
        // 移除占位消息
        removeThreadMessage(currentBinding.placeholderMessageId);
        clearMessageTarget(currentBinding.placeholderMessageId);
        // 清理请求绑定
        clearRequestBinding(requestId);
        clearPendingRequest(requestId);
        markMessageComplete(currentBinding.placeholderMessageId);
        // 显示超时错误提示
        addToast('error', '等待响应超时，请重试');
      }
    }, 60000); // 60 秒超时

    if (!binding) {
      createRequestBinding({
        requestId,
        userMessageId,
        placeholderMessageId: standard.id,
        createdAt: standard.timestamp || Date.now(),
        timeoutId,
      });
    } else {
      // 清除旧的超时定时器
      if (binding.timeoutId) {
        clearTimeout(binding.timeoutId);
      }
      updateRequestBinding(requestId, { placeholderMessageId: standard.id, userMessageId, timeoutId });
    }
    addPendingRequest(requestId);
    // 🔧 立即渲染占位消息：与真实消息保持一致的卡片结构，仅显示底部三点动画
    // 提供“发送即响应”的体验，避免等待首 token 才出现卡片
    upsertThreadMessage(uiMessage);
    // 🔧 为占位消息建立路由，确保流式更新可以命中同一条消息
    routeStandardMessage(standard);
    if (uiMessage.isStreaming) {
      markMessageActive(uiMessage.id);
    }
    // 🔧 回放可能提前到达的流式更新
    flushPendingStreamUpdates(standard.id);
    return;
  }

  if (isUserMessage) {
    if (requestId) {
      const placeholderMessageId = meta?.placeholderMessageId as string | undefined;
      const binding = getRequestBinding(requestId);
      if (!binding && placeholderMessageId) {
        createRequestBinding({
          requestId,
          userMessageId: standard.id,
          placeholderMessageId,
          createdAt: standard.timestamp || Date.now(),
        });
      } else if (binding) {
        updateRequestBinding(requestId, { userMessageId: standard.id });
      }
    }
    upsertThreadMessage(uiMessage);
    return;
  }

  if (!uiMessage.isStreaming && !hasRenderableContent(uiMessage)) {
    console.warn('[MessageHandler] 消息不可渲染，跳过:', { id: standard.id, uiMessage });
    // 🔧 改为警告而非抛出错误，避免中断消息处理
    return;
  }

  // === 检查是否有对应的占位消息需要替换 ===
  if (requestId) {
    const binding = getRequestBinding(requestId);
    console.log('[MessageHandler] 检查占位消息绑定:', {
      requestId,
      hasBinding: !!binding,
      realMessageId: binding?.realMessageId,
      placeholderMessageId: binding?.placeholderMessageId,
    });

    if (binding && !binding.realMessageId) {
      // 首次收到真实消息，需要原地替换占位消息
      // 🔧 清除超时定时器（已收到真实消息）
      if (binding.timeoutId) {
        clearTimeout(binding.timeoutId);
      }
      const placeholderId = binding.placeholderMessageId;
      const existingPlaceholder = getState().threadMessages.find(m => m.id === placeholderId);
      if (!existingPlaceholder) {
        throw new Error(`[MessageHandler] 未找到占位消息: ${placeholderId}`);
      }

      if (placeholderId !== standard.id) {
        throw new Error(`[MessageHandler] 首条响应消息 ID 不匹配占位消息: placeholder=${placeholderId} real=${standard.id}`);
      }

      if (!hasRenderableContent(uiMessage)) {
        return;
      }

      // 🔧 统一消息 ID：占位消息即真实消息，直接在同一条消息上更新
      updateRequestBinding(requestId, { realMessageId: standard.id, placeholderMessageId: standard.id });
      const mergedMessage: import('../types/message').Message = {
        ...existingPlaceholder,
        ...uiMessage,
        metadata: {
          ...(existingPlaceholder.metadata || {}),
          ...(uiMessage.metadata || {}),
          isPlaceholder: false,
          wasPlaceholder: true,
          placeholderState: undefined,
          requestId,
        },
      };
      updateThreadMessage(placeholderId, mergedMessage);

      // 标记为活跃消息
      if (uiMessage.isStreaming) {
        markMessageActive(placeholderId);
      }

      // 可能存在提前到达的流式更新，立即补齐
      flushPendingStreamUpdates(standard.id);

      return;
    }
  }

  // === 后续消息处理（非首次或无占位消息关联） ===
  const target = routeStandardMessage(standard);

  // 🔧 调试日志：追踪 Worker 消息路由
  console.log('[DEBUG] handleContentMessage: 消息路由', {
    id: standard.id,
    source: standard.source,
    agent: standard.agent,
    category: standard.category,
    targetLocation: target.location,
    targetWorker: (target as { worker?: string }).worker,
  });

  if (target.location === 'none' || target.location === 'task') {
    return;
  }

  // 🔧 修复：流式消息需要标记为活跃，驱动 isProcessing 状态
  if (uiMessage.isStreaming) {
    markMessageActive(uiMessage.id);
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

    // 🔧 可能存在提前到达的流式更新，立即补齐
    flushPendingStreamUpdates(standard.id);
}


function handleStandardUpdate(message: WebviewMessage) {
  const rawUpdate = message.update as StreamUpdate;
  if (!rawUpdate?.messageId || !rawUpdate.messageId.trim()) {
    throw new Error('[MessageHandler] 流式更新缺少 messageId');
  }
  const update = rawUpdate;

  // 查找路由
  const location = getMessageTarget(update.messageId);

  if (!location) {
    console.warn(`[MessageHandler] 未找到流式更新的路由，暂存更新: ${update.messageId}`);
    queueStreamUpdate(update);
    return;
  }

  const applied = applyUpdateToLocation(location, update);
  if (!applied) {
    queueStreamUpdate(update);
  }
}

function handleStandardComplete(message: WebviewMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) {
    throw new Error('[MessageHandler] unifiedComplete 缺少 message');
  }
  const standard = assertStandardMessageId(rawStandard);

  console.log('[MessageHandler] handleStandardComplete 开始:', {
    id: standard.id,
    category: standard.category,
    lifecycle: standard.lifecycle,
    requestId: (standard.metadata as Record<string, unknown> | undefined)?.requestId,
  });

  // 🔧 根治：只处理 CONTENT 类别的消息，其他类别直接跳过
  // DATA/CONTROL/NOTIFY 消息不应该进入对话列表
  if (standard.category !== MessageCategory.CONTENT) {
    console.log('[MessageHandler] handleStandardComplete: 非 CONTENT 类别，跳过');
    return;
  }

  const requestId = (standard.metadata as Record<string, unknown> | undefined)?.requestId as string | undefined;
  const actualMessageId = standard.id;
  const location = getMessageTarget(actualMessageId);
  console.log('[MessageHandler] handleStandardComplete 路由查找:', {
    originalId: standard.id,
    actualMessageId,
    hasLocation: !!location,
    locationInfo: location,
  });
  if (!location) {
    throw new Error(`[MessageHandler] 完成消息缺少路由: ${standard.id}`);
  }

  if (location.location === 'none' || location.location === 'task') {
    return;
  }

  // 🔧 修复：先检查消息是否存在，使用 actualMessageId
  // complete 消息是用来"完成"已有消息的
  let messageExists = false;
  if (location.location === 'thread') {
    messageExists = getState().threadMessages.some(m => m.id === actualMessageId);
  } else if (location.location === 'worker') {
    messageExists = getState().agentOutputs[location.worker].some(m => m.id === actualMessageId);
  } else if (location.location === 'both') {
    messageExists = getState().threadMessages.some(m => m.id === actualMessageId) ||
                    getState().agentOutputs[location.worker].some(m => m.id === actualMessageId);
  }

  if (!messageExists) {
    throw new Error(`[MessageHandler] 完成消息未找到对应卡片: ${actualMessageId}`);
  }

  // 🔧 修复：消息完成时标记为非活跃，驱动 isProcessing 状态
  markMessageComplete(actualMessageId);

  const uiMessage = mapStandardMessage(standard);
  const hasContent = hasRenderableContent(uiMessage);

  // 保留已有内容：complete 消息可能没有 blocks/content
  const getExistingMessage = () => {
    if (location.location === 'thread') {
      return getState().threadMessages.find(m => m.id === actualMessageId);
    }
    if (location.location === 'worker') {
      return getState().agentOutputs[location.worker].find(m => m.id === actualMessageId);
    }
    if (location.location === 'both') {
      return getState().threadMessages.find(m => m.id === actualMessageId)
        || getState().agentOutputs[location.worker].find(m => m.id === actualMessageId);
    }
    return undefined;
  };

  const existingMessage = getExistingMessage();
  const baseMessage = hasContent ? uiMessage : (existingMessage || uiMessage);
  if (!baseMessage) {
    clearMessageTarget(actualMessageId);
    return;
  }

  const shouldConvertFromPlaceholder = Boolean(existingMessage?.metadata?.isPlaceholder)
    && hasRenderableContent(baseMessage);

  // 添加完成动画标记，并确保流式结束
  const completedMessage = {
    ...baseMessage,
    id: actualMessageId, // 使用实际的消息 ID
    isStreaming: false,
    isComplete: true,
    metadata: {
      ...(baseMessage.metadata || {}),
      justCompleted: true,
      ...(shouldConvertFromPlaceholder
        ? {
            isPlaceholder: false,
            wasPlaceholder: true,
            placeholderState: undefined,
          }
        : {}),
    },
  };

  // 更新已存在的消息
  if (location.location === 'thread') {
    updateThreadMessage(actualMessageId, completedMessage);
  } else if (location.location === 'worker') {
    updateAgentMessage(location.worker, actualMessageId, completedMessage);
  } else if (location.location === 'both') {
    updateThreadMessage(actualMessageId, completedMessage);
    updateAgentMessage(location.worker, actualMessageId, completedMessage);
  }

  // 🔧 补齐可能提前到达的流式更新
  flushPendingStreamUpdates(actualMessageId);

  // 清理请求绑定
  if (requestId) {
    // 延迟清理，确保动画完成
    setTimeout(() => {
      clearRequestBinding(requestId);
    }, 1000);
  }

  // 移除 justCompleted 标记（动画完成后）
  setTimeout(() => {
    const cleanedMessage = {
      ...completedMessage,
      metadata: {
        ...(completedMessage.metadata || {}),
        justCompleted: false,
      },
    };
    if (location.location === 'thread' || location.location === 'both') {
      updateThreadMessage(actualMessageId, cleanedMessage);
    }
    if (location.location === 'worker' || location.location === 'both') {
      updateAgentMessage(location.worker, actualMessageId, cleanedMessage);
    }
  }, 500);

  clearMessageTarget(actualMessageId);
}


/**
 * 🔧 统一消息通道：处理控制消息
 *
 * 控制消息通过 MessageHub.sendControl() 发送，包含 controlType 和 payload
 */
function handleUnifiedControlMessage(standard: StandardMessage) {
  if (!standard.control) {
    throw new Error('[MessageHandler] 控制消息缺少 control 字段');
  }

  const { controlType, payload } = standard.control as {
    controlType: string;
    payload: Record<string, unknown>;
  };

  switch (controlType) {
    case 'phase_changed':
      // 阶段变化：仅同步运行态（不再展示阶段步骤）
      {
        const isRunning = payload?.isRunning as boolean | undefined;
        if (typeof isRunning === 'boolean') {
          if (isRunning) {
            setIsProcessing(true);
          } else {
            clearProcessingState();
          }
        }
      }
      break;

    case 'task_accepted': {
      // 🔧 防御性检查：只有当 backendProcessing 已为 true 时才清除 pending
      // 正常时序：processingStateChanged:true → backendProcessing=true → task_accepted → 清除 pending
      // 如果 backendProcessing 仍为 false，说明时序异常，先设置处理状态
      const requestId = payload?.requestId as string | undefined;
      if (requestId) {
        if (!getBackendProcessing()) {
          // 异常时序：先确保处理状态为 true，避免 isProcessing 出现空窗期
          setIsProcessing(true);
        }
        clearPendingRequest(requestId);

        // 更新占位消息状态：pending → received
        const binding = getRequestBinding(requestId);
        if (binding) {
          const placeholder = getState().threadMessages.find(m => m.id === binding.placeholderMessageId);
          const baseMetadata = (placeholder?.metadata && typeof placeholder.metadata === 'object')
            ? placeholder.metadata
            : {};
          updateThreadMessage(binding.placeholderMessageId, {
            metadata: {
              ...baseMetadata,
              isPlaceholder: true,
              placeholderState: 'received',
              requestId,
            },
          });
        }
      }
      break;
    }

    case 'task_rejected': {
      const requestId = payload?.requestId as string | undefined;
      if (requestId) {
        clearPendingRequest(requestId);
      }
      break;
    }

    case 'task_started':
      // 任务开始执行
      setIsProcessing(true);
      {
        const requestId = payload?.requestId as string | undefined;
        if (requestId) {
          const binding = getRequestBinding(requestId);
          if (binding) {
            const placeholder = getState().threadMessages.find(m => m.id === binding.placeholderMessageId);
            const baseMetadata = (placeholder?.metadata && typeof placeholder.metadata === 'object')
              ? placeholder.metadata
              : {};
            updateThreadMessage(binding.placeholderMessageId, {
              metadata: {
                ...baseMetadata,
                isPlaceholder: true,
                placeholderState: 'thinking',
                requestId,
              },
            });
          }
        }
      }
      break;

    case 'task_completed':
    case 'task_failed':
      // 任务完成/失败：清理状态
      clearProcessingState();
      break;

    case 'worker_status': {
      // Worker 状态更新：从控制消息同步状态到 UI
      const store = getState();
      const worker = payload?.worker as string | undefined;
      const available = payload?.available as boolean | undefined;
      if (worker && typeof available === 'boolean') {
        store.modelStatus = {
          ...store.modelStatus,
          [worker]: { status: available ? 'available' : 'unavailable' },
        };
      }
      break;
    }

    default:
      throw new Error(`[MessageHandler] 未知控制消息类型: ${controlType}`);
  }
}

function handleUnifiedNotify(standard: StandardMessage) {
  const level = standard.notify?.level || 'info';
  const content = extractTextFromStandardBlocks(standard.blocks);
  if (!content) {
    throw new Error('[MessageHandler] 通知消息缺少内容');
  }
  addToast(level, content);
}

function handleUnifiedData(standard: StandardMessage) {
  const data = standard.data;
  if (!data) {
    throw new Error('[MessageHandler] 数据消息缺少 data 字段');
  }
  const { dataType, payload } = data;
  const asMessage = (extra: Record<string, unknown>) => ({ ...extra } as WebviewMessage);

  switch (dataType) {
    case 'stateUpdate':
      handleStateUpdate(asMessage({ state: payload.state }));
      break;

    case 'processingStateChanged': {
      const isProcessing = payload.isProcessing as boolean | undefined;
      if (typeof isProcessing === 'boolean') {
        setIsProcessing(isProcessing);
      }
      const source = payload.source as string | undefined;
      const agent = payload.agent as string | undefined;
      if (source) {
        setProcessingActor(source, agent);
      }
      break;
    }

    case 'sessionsUpdated':
      handleSessionsUpdated(asMessage({ sessions: payload.sessions }));
      break;

    case 'sessionCreated':
    case 'sessionLoaded':
    case 'sessionSwitched':
      handleSessionChanged(asMessage({
        sessionId: payload.sessionId,
        session: payload.session
      }));
      break;

    case 'sessionMessagesLoaded':
      handleSessionMessagesLoaded(asMessage({
        sessionId: payload.sessionId,
        messages: payload.messages,
        workerMessages: payload.workerMessages
      }));
      break;

    case 'confirmationRequest':
      handleConfirmationRequest(asMessage(payload));
      break;

    case 'recoveryRequest':
      handleRecoveryRequest(asMessage(payload));
      break;

    case 'questionRequest':
      handleQuestionRequest(asMessage(payload));
      break;

    case 'clarificationRequest':
      handleClarificationRequest(asMessage(payload));
      break;

    case 'workerQuestionRequest':
      handleWorkerQuestionRequest(asMessage(payload));
      break;

    case 'toolAuthorizationRequest':
      handleToolAuthorizationRequest(asMessage(payload));
      break;

    case 'missionPlanned':
      handleMissionPlanned(asMessage(payload));
      break;

    case 'assignmentPlanned':
      handleAssignmentPlanned(asMessage(payload));
      break;

    case 'assignmentStarted':
      handleAssignmentStarted(asMessage(payload));
      break;

    case 'assignmentCompleted':
      handleAssignmentCompleted(asMessage(payload));
      break;

    case 'todoStarted':
      handleTodoStarted(asMessage(payload));
      break;

    case 'todoCompleted':
      handleTodoCompleted(asMessage(payload));
      break;

    case 'todoFailed':
      handleTodoFailed(asMessage(payload));
      break;

    case 'dynamicTodoAdded':
      handleDynamicTodoAdded(asMessage(payload));
      break;

    case 'todoApprovalRequested':
      handleTodoApprovalRequested(asMessage(payload));
      break;

    case 'workerSessionCreated':
      handleWorkerSessionCreated(asMessage(payload));
      break;

    case 'workerSessionResumed':
      handleWorkerSessionResumed(asMessage(payload));
      break;

    case 'workerStatusUpdate':
      handleWorkerStatusUpdate(asMessage(payload));
      break;

    case 'interactionModeChanged':
      getState().interactionMode = (payload.mode as string) === 'ask' ? 'ask' : 'auto';
      break;

    case 'missionExecutionFailed':
      clearProcessingState();
      break;

    case 'missionFailed':
      clearProcessingState();
      break;

    default:
      break;
  }
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

    // 如果是不同的会话，清空当前消息和请求绑定
    if (currentId !== newSessionId) {
      clearAllMessages();
      clearMessageTargets();
      clearAllRequestBindings();
    }

    setCurrentSessionId(newSessionId);
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
    const formatMessage = (m: any): Message => {
      const id = typeof m?.id === 'string' && m.id.trim() ? m.id.trim() : '';
      if (!id) {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息缺少 id');
      }
      const role = m?.role;
      if (role !== 'user' && role !== 'assistant' && role !== 'system') {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息 role 无效');
      }
      if (typeof m?.content !== 'string') {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息 content 非字符串');
      }
      if (typeof m?.timestamp !== 'number') {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息 timestamp 无效');
      }
      return {
        id,
        role,
        content: m.content,
        source: m.source || 'orchestrator',
        timestamp: m.timestamp,
        isStreaming: false,
        isComplete: true,
        blocks: mapStandardBlocks(
          (Array.isArray(m.blocks) && m.blocks.length > 0)
            ? m.blocks
            : [{
                type: 'text' as const,
                content: m.content || '',
              }]
        ),
      };
    };

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
    // auto 模式下静默跳过澄清，不显示 toast
    vscode.postMessage({
      type: 'answerClarification',
      answers: null,
      additionalInfo: null,
      autoSkipped: true,  // 标记为自动跳过
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
  const missionId = typeof message.missionId === 'string' && message.missionId.trim() ? message.missionId.trim() : '';
  if (!missionId) {
    throw new Error('[MessageHandler] MissionPlanned 缺少 missionId');
  }
  const assignments = ensureArray(message.assignments) as any[];
  const assignmentSeen = new Set<string>();
  const mappedAssignments: AssignmentPlan[] = assignments
    .filter((assignment) => assignment && typeof assignment === 'object')
    .map((assignment) => {
      const assignmentId = typeof assignment.id === 'string' && assignment.id.trim() ? assignment.id.trim() : '';
      if (!assignmentId) {
        throw new Error('[MessageHandler] MissionPlanned assignment 缺少 id');
      }
      if (assignmentSeen.has(assignmentId)) {
        throw new Error(`[MessageHandler] MissionPlanned assignment id 重复: ${assignmentId}`);
      }
      assignmentSeen.add(assignmentId);
      const todoSeen = new Set<string>();
      const todos = ensureArray(assignment.todos)
        .filter((todo: any) => !!todo && typeof todo === 'object')
        .map((todo: any) => {
          const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
          if (!todoId) {
            throw new Error('[MessageHandler] MissionPlanned todo 缺少 id');
          }
          if (todoSeen.has(todoId)) {
            throw new Error(`[MessageHandler] MissionPlanned todo id 重复: ${todoId}`);
          }
          todoSeen.add(todoId);
          return {
            id: todoId,
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
          } as AssignmentTodo;
        });
      return {
        id: assignmentId,
        workerId: assignment.workerId,
        responsibility: assignment.responsibility,
        status: assignment.status,
        progress: assignment.progress,
        todos,
      };
    });
  const plan: MissionPlan = { missionId, assignments: mappedAssignments };
  setMissionPlan(plan);
}

function handleAssignmentPlanned(message: WebviewMessage) {
  const assignmentId = typeof message.assignmentId === 'string' && message.assignmentId.trim()
    ? message.assignmentId.trim()
    : '';
  if (!assignmentId) {
    throw new Error('[MessageHandler] AssignmentPlanned 缺少 assignmentId');
  }
  const todoSeen = new Set<string>();
  const todos = ensureArray(message.todos)
    .filter((todo: any) => !!todo && typeof todo === 'object')
    .map((todo: any) => {
      const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
      if (!todoId) {
        throw new Error('[MessageHandler] AssignmentPlanned todo 缺少 id');
      }
      if (todoSeen.has(todoId)) {
        throw new Error(`[MessageHandler] AssignmentPlanned todo id 重复: ${todoId}`);
      }
      todoSeen.add(todoId);
      return {
        id: todoId,
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
      };
    });

  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    todos,
  }));
}

function handleAssignmentStarted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] AssignmentStarted 缺少 assignmentId');
  }
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    status: 'running',
  }));
}

function handleAssignmentCompleted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] AssignmentCompleted 缺少 assignmentId');
  }
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
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoStarted 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoStarted 缺少 todoId');
  }
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'in_progress',
  }));
}

function handleTodoCompleted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoCompleted 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoCompleted 缺少 todoId');
  }
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'completed',
  }));
}

function handleTodoFailed(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoFailed 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoFailed 缺少 todoId');
  }
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'failed',
  }));
}

function handleDynamicTodoAdded(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] DynamicTodoAdded 缺少 assignmentId');
  }
  const todo = message.todo as any;
  if (!todo || typeof todo !== 'object') {
    throw new Error('[MessageHandler] DynamicTodoAdded 缺少 todo');
  }
  const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
  if (!todoId) {
    throw new Error('[MessageHandler] DynamicTodoAdded todo 缺少 id');
  }
  const newTodo: AssignmentTodo = {
    id: todoId,
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
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoApprovalRequested 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoApprovalRequested 缺少 todoId');
  }
  if (!reason || !reason.trim()) {
    throw new Error('[MessageHandler] TodoApprovalRequested 缺少 reason');
  }
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    approvalStatus: 'pending',
    approvalNote: reason,
  }));
}

function mapStandardMessage(standard: StandardMessage): Message {
  const blocks = mapStandardBlocks(standard.blocks || []);
  const content = blocksToContent(blocks);
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

  const dispatchToWorker = Boolean(baseMetadata.dispatchToWorker);

  const explicitRole = (baseMetadata as { role?: unknown } | undefined)?.role;
  const resolvedRole =
    explicitRole === 'user'
      ? 'user'
      : (isSystemNotice ? 'system' : 'assistant');

  return {
    id: standard.id,
    role: resolvedRole,
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
  const list = ensureArray<StandardContentBlock>(blocks);
  const invalid = list.filter((block) => !block || typeof block !== 'object' || !('type' in block));
  if (invalid.length > 0) {
    throw new Error('[MessageHandler] 标准消息块无效');
  }
  return list.map((block) => {
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
  const safeExisting = ensureArray(existing).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  const safeIncoming = ensureArray(incoming).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  const next = [...safeExisting];
  for (const block of safeIncoming) {
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
 * 将检测到的模型状态同步到全局 store，供 BottomTabs 和 SettingsPanel 共用
 */
function handleWorkerStatusUpdate(message: WebviewMessage) {
  const statuses = message.statuses as ModelStatusMap;
  if (!statuses) return;

  const store = getState();

  // 直接存储完整的状态信息，不再简化
  // 这样 BottomTabs 和 SettingsPanel 可以使用同一个数据源
  store.modelStatus = { ...store.modelStatus, ...statuses };
}

// ============ Worker Session 事件处理（提案 4.1） ============

function handleWorkerSessionCreated(message: WebviewMessage) {
  const sessionId = (message.sessionId as string) || '';
  const assignmentId = (message.assignmentId as string) || '';
  const workerId = (message.workerId as string) || '';

  if (!sessionId) {
    throw new Error('[MessageHandler] WorkerSessionCreated 缺少 sessionId');
  }
  if (!assignmentId) {
    throw new Error('[MessageHandler] WorkerSessionCreated 缺少 assignmentId');
  }
  if (!workerId) {
    throw new Error('[MessageHandler] WorkerSessionCreated 缺少 workerId');
  }

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
  const workerId = (message.workerId as string) || '';

  if (!sessionId) {
    throw new Error('[MessageHandler] WorkerSessionResumed 缺少 sessionId');
  }
  if (!assignmentId) {
    throw new Error('[MessageHandler] WorkerSessionResumed 缺少 assignmentId');
  }
  if (!workerId) {
    throw new Error('[MessageHandler] WorkerSessionResumed 缺少 workerId');
  }

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
      workerId,
      isResumed: true,
      completedTodos,
    };
    addWorkerSession(session);
  }

  // 系统通知由 MessageHub 下发，前端不再本地创建
}

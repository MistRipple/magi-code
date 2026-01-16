/**
 * Orchestrator Normalizer - 编排器消息标准化
 *
 * 将 OrchestratorAgent 的 emitUIMessage 转换为标准消息格式
 * 实现编排器消息与 CLI 消息的统一处理
 */

import { v4 as uuidv4 } from 'uuid';
import {
  StandardMessage,
  StreamUpdate,
  MessageLifecycle,
  MessageType,
  ContentBlock,
  TextBlock,
} from '../protocol';
import { OrchestratorUIMessage, OrchestratorState } from '../orchestrator/protocols/types';

/** 编排器消息类型到内容块类型的映射 */
const MESSAGE_TYPE_MAP: Record<string, ContentBlock['type']> = {
  progress_update: 'text',
  plan_ready: 'text',
  summary: 'text',
  direct_response: 'text',
  confirmationRequest: 'text',
  error: 'text',
};

/**
 * 将 OrchestratorUIMessage 转换为 StandardMessage
 */
export function normalizeOrchestratorMessage(
  uiMessage: OrchestratorUIMessage,
  traceId?: string
): StandardMessage {
  const messageId = `msg-orch-${uuidv4().substring(0, 8)}`;

  // 构建内容块
  const blocks: ContentBlock[] = [];

  // 主文本内容
  if (uiMessage.content) {
    const textBlock: TextBlock = {
      type: 'text',
      content: uiMessage.content,
      isMarkdown: uiMessage.type === 'plan_ready',
    };
    blocks.push(textBlock);
  }

  // 如果有计划信息，添加为额外块
  if (uiMessage.metadata?.formattedPlan) {
    const planBlock: TextBlock = {
      type: 'text',
      content: uiMessage.metadata.formattedPlan,
      isMarkdown: true,
    };
    blocks.push(planBlock);
  }

  // 确定消息类型
  let messageType: MessageType = MessageType.TEXT;
  if (uiMessage.type === 'error') {
    messageType = MessageType.ERROR;
  } else if (uiMessage.type === 'progress_update') {
    messageType = MessageType.PROGRESS;
  } else if (uiMessage.type === 'plan_ready') {
    messageType = MessageType.PLAN;
  } else if (uiMessage.type === 'summary') {
    messageType = MessageType.RESULT;
  }

  // 确定生命周期状态
  let lifecycle: MessageLifecycle = MessageLifecycle.COMPLETED;
  if (uiMessage.type === 'progress_update') {
    lifecycle = MessageLifecycle.STREAMING;
  } else if (uiMessage.type === 'error') {
    lifecycle = MessageLifecycle.FAILED;
  }

  return {
    id: messageId,
    traceId: traceId || `trace-${uuidv4().substring(0, 8)}`,
    type: messageType,
    cli: 'claude', // 编排器使用 claude
    source: 'orchestrator',
    lifecycle,
    timestamp: uiMessage.timestamp || Date.now(),
    updatedAt: Date.now(),
    blocks,
    metadata: {
      taskId: uiMessage.taskId,
      phase: uiMessage.metadata?.phase,
      subTaskId: uiMessage.metadata?.subTaskId,
    },
  };
}

/**
 * 创建编排器进度更新
 */
export function createOrchestratorUpdate(
  messageId: string,
  content: string,
  _phase?: OrchestratorState,
  _progress?: number
): StreamUpdate {
  return {
    messageId,
    timestamp: Date.now(),
    updateType: 'append',
    appendText: content,
  };
}

/**
 * 判断是否为编排器状态消息（不需要显示给用户）
 */
export function isInternalStateMessage(uiMessage: OrchestratorUIMessage): boolean {
  const internalPatterns = [
    /^正在分析任务依赖关系/,
    /^执行模式已调整/,
    /^已移除冗余/,
    /^编排纪律提示/,
  ];

  return internalPatterns.some(pattern => pattern.test(uiMessage.content));
}

/**
 * 获取消息优先级（用于批处理排序）
 */
export function getMessagePriority(uiMessage: OrchestratorUIMessage): number {
  const priorities: Record<string, number> = {
    error: 100,
    summary: 90,
    plan_ready: 80,
    direct_response: 70,
    confirmationRequest: 60,
    progress_update: 50,
  };

  return priorities[uiMessage.type] || 0;
}


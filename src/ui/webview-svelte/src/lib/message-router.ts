import type { DisplayTarget } from '../types/message-routing';
import { classifyMessage } from './message-classifier';
import { resolveDisplayTarget } from '../config/routing-table';
import type { StandardMessage } from '../../../../protocol/message-protocol';

const messageTargetMap = new Map<string, DisplayTarget>();

/**
 * 检查当前是否为调试模式
 * 从 window 或 vscode state 读取调试模式状态
 */
function isDebugMode(): boolean {
  // 优先从 window 全局变量读取
  if (typeof window !== 'undefined') {
    // 检查 window 上的调试标志
    if ((window as unknown as Record<string, unknown>).__DEBUG_MODE__ === true) {
      return true;
    }
    // 检查 localStorage 中的调试模式设置
    try {
      const debugSetting = localStorage.getItem('multicli:debugMode');
      if (debugSetting === 'true') {
        return true;
      }
    } catch {
      // localStorage 不可用时忽略
    }
  }
  return false;
}

/**
 * 根据 visibility 字段检查消息是否应该展示
 * 返回 null 表示继续正常路由，返回 DisplayTarget 表示直接返回该结果
 */
function checkVisibility(standard: StandardMessage): DisplayTarget | null {
  const visibility = standard.visibility;

  // visibility === 'system' → 不展示给用户
  if (visibility === 'system') {
    return { location: 'none', reason: 'system-visibility' };
  }

  // visibility === 'debug' → 仅调试模式可见
  if (visibility === 'debug' && !isDebugMode()) {
    return { location: 'none', reason: 'debug-only' };
  }

  // visibility === 'user' 或 undefined → 继续正常路由
  return null;
}

export function routeStandardMessage(standard: StandardMessage): DisplayTarget {
  // 1. 首先检查 visibility 字段
  const visibilityTarget = checkVisibility(standard);
  if (visibilityTarget !== null) {
    messageTargetMap.set(standard.id, visibilityTarget);
    return visibilityTarget;
  }

  // 2. 继续正常路由
  const { category, worker } = classifyMessage(standard);
  const target = resolveDisplayTarget(category, worker);
  messageTargetMap.set(standard.id, target);
  return target;
}

export function getMessageTarget(messageId: string): DisplayTarget | null {
  return messageTargetMap.get(messageId) || null;
}

export function clearMessageTargets(): void {
  messageTargetMap.clear();
}

export function clearMessageTarget(messageId: string): void {
  messageTargetMap.delete(messageId);
}

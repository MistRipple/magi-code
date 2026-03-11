/**
 * CommandHandler 接口 - 消息路由拆分基础设施（P1-3 修复）
 *
 * 将 WebviewProvider 的 91 个 switch-case 分支按业务域拆分为独立 Handler，
 * 每个 Handler 注册自己关心的 message type 并处理对应消息。
 */

import type * as vscode from 'vscode';
import type { WebviewToExtensionMessage } from '../../types';
import type { DataMessageType, NotifyLevel } from '../../protocol/message-protocol';
import type { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import type { MissionDrivenEngine } from '../../orchestrator/core';
import type { ProjectKnowledgeBase } from '../../knowledge/project-knowledge-base';
import type { PromptEnhancerService } from '../../services/prompt-enhancer-service';

/**
 * CommandHandlerContext - Handler 的依赖注入接口
 *
 * 暴露 WebviewProvider 中 Handler 需要的公共服务，
 * 避免 Handler 直接持有 WebviewProvider 引用。
 */
export interface CommandHandlerContext {
  sendData(dataType: DataMessageType, payload: Record<string, unknown>): void;
  sendToast(message: string, level?: NotifyLevel, duration?: number): void;
  sendStateUpdate(): void;
  getAdapterFactory(): IAdapterFactory;
  getOrchestratorEngine(): MissionDrivenEngine;
  getProjectKnowledgeBase(): ProjectKnowledgeBase | undefined;
  getWorkspaceRoot(): string;
  getPromptEnhancer(): PromptEnhancerService;
  getExtensionUri(): vscode.Uri;
  /** 配置变更后刷新模型状态（清除缓存 + 强制重新检测） */
  refreshWorkerStatus(): void;
}

/**
 * CommandHandler - 消息处理器接口
 *
 * 每个 Handler 声明自己支持的 message type 集合，
 * handleMessage 方法通过查找匹配的 Handler 进行委派。
 */
export interface CommandHandler {
  readonly supportedTypes: ReadonlySet<string>;
  handle(message: WebviewToExtensionMessage, ctx: CommandHandlerContext): Promise<void>;
}

/**
 * 智能编排器 - 独立编排者架构
 *
 * 架构重构：
 * - Orchestrator Claude：专职编排，不执行任何编码任务
 * - Worker Agents：专职执行，向编排者汇报进度和结果
 */

import { CLIType, InteractionMode, INTERACTION_MODE_CONFIGS, InteractionModeConfig } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { TaskManager } from '../task-manager';
import { SnapshotManager } from '../snapshot-manager';
import { globalEventBus } from '../events';
import { OrchestratorAgent, ConfirmationCallback } from './orchestrator-agent';
import { VerificationRunner, VerificationConfig } from './verification-runner';
import {
  ExecutionPlan,
  ExecutionResult,
  SubTask,
  OrchestratorState,
  QuestionCallback,
} from './protocols/types';
import { formatPlanForUser } from './prompts/orchestrator-prompts';

// 重新导出类型以保持向后兼容
export type { ExecutionPlan, ExecutionResult, SubTask };
export { ConfirmationCallback };

/** 子任务计划（向后兼容） */
export interface SubTaskPlan {
  id: string;
  description: string;
  assignedCli: CLIType;
  reason: string;
  targetFiles?: string[];
  dependencies: string[];
  prompt: string;
}

/** 编排器配置 */
export interface OrchestratorConfig {
  timeout: number;
  /** 空闲超时时间（毫秒） */
  idleTimeout?: number;
  /** 最大执行超时时间（毫秒） */
  maxTimeout?: number;
  verification?: Partial<VerificationConfig>;
  maxRetries: number;
  integration?: {
    enabled?: boolean;
    maxRounds?: number;
    worker?: CLIType;
  };
}

/** 编排器状态 */
export type OrchestratorPhase = OrchestratorState;

/** 恢复确认回调类型 */
export type RecoveryConfirmationCallback = (
  failedTask: any,
  error: string,
  options: { retry: boolean; rollback: boolean }
) => Promise<'retry' | 'rollback' | 'continue'>;

const DEFAULT_CONFIG: OrchestratorConfig = {
  timeout: 300000,
  maxRetries: 3,
  integration: {
    enabled: true,
    maxRounds: 2,
    worker: 'claude',
  },
};

/**
 * 智能编排器 - 基于独立编排者架构
 */
export class IntelligentOrchestrator {
  private cliFactory: CLIAdapterFactory;
  private taskManager: TaskManager;
  private snapshotManager: SnapshotManager;
  private config: OrchestratorConfig;
  private workspaceRoot: string;

  // 核心：独立编排者 Agent
  private orchestratorAgent: OrchestratorAgent;

  // 交互模式
  private interactionMode: InteractionMode = 'agent';
  private readonly directAnswerKeywords = [
    '是什么', '为什么', '怎么', '如何', '能否', '可以吗', '建议', '解释', '了解', '对比', '优缺点',
    '方案', '思路', '总结', '概念', '原理', '问题', '是否', '推荐'
  ];
  private readonly taskIntentKeywords = [
    '实现', '添加', '新增', '修改', '修复', '重构', '迁移', '集成', '优化', '部署', '测试', '生成',
    '创建', '删除', '更新', '写', '改', '开发', '搭建', '编排', '完善'
  ];
  private modeConfig: InteractionModeConfig = INTERACTION_MODE_CONFIGS.agent;

  // 验证器
  private verificationRunner: VerificationRunner | null = null;

  // 状态
  private isRunning = false;
  private currentTaskId: string | null = null;
  private abortController: AbortController | null = null;
  private statusUpdateInterval: NodeJS.Timeout | null = null;

  constructor(
    cliFactory: CLIAdapterFactory,
    taskManager: TaskManager,
    snapshotManager: SnapshotManager,
    workspaceRoot: string,
    config?: Partial<OrchestratorConfig>
  ) {
    this.cliFactory = cliFactory;
    this.taskManager = taskManager;
    this.snapshotManager = snapshotManager;
    this.workspaceRoot = workspaceRoot;
    this.config = { ...DEFAULT_CONFIG, ...config };

    // 创建独立编排者 Agent，传递 workspaceRoot 和 snapshotManager 以支持验证和回滚功能
    this.orchestratorAgent = new OrchestratorAgent(
      cliFactory,
      {
        timeout: this.config.timeout,
        maxRetries: this.config.maxRetries,
        verification: this.config.verification,
        integration: this.config.integration,
      },
      workspaceRoot,
      snapshotManager,
      taskManager
    );

    this.setupOrchestratorEvents();
  }

  /** 设置编排者事件监听 */
  private setupOrchestratorEvents(): void {
    this.orchestratorAgent.on('stateChange', (state: OrchestratorState) => {
      globalEventBus.emitEvent('orchestrator:phase_changed', {
        taskId: this.currentTaskId || undefined,
        data: { phase: state, isRunning: this.isRunning },
      });
    });

    this.orchestratorAgent.on('uiMessage', (message) => {
      if (message.type === 'worker_output') {
        globalEventBus.emitEvent('cli:output', {
          taskId: this.currentTaskId || undefined,
          data: { cli: message.metadata?.workerType, chunk: message.content },
        });
      }
    });
  }

  /** 设置交互模式 */
  setInteractionMode(mode: InteractionMode): void {
    this.interactionMode = mode;
    this.modeConfig = INTERACTION_MODE_CONFIGS[mode];
    console.log(`[IntelligentOrchestrator] 交互模式设置为: ${mode}`);
    globalEventBus.emitEvent('orchestrator:mode_changed', { data: { mode } });
  }

  /** 获取当前交互模式 */
  getInteractionMode(): InteractionMode {
    return this.interactionMode;
  }

  /** 设置用户确认回调 */
  setConfirmationCallback(callback: ConfirmationCallback): void {
    this.orchestratorAgent.setConfirmationCallback(callback);
  }

  /** 设置用户补充问题回调 */
  setQuestionCallback(callback: QuestionCallback): void {
    this.orchestratorAgent.setQuestionCallback(callback);
  }

  /** 设置恢复确认回调（向后兼容） */
  setRecoveryConfirmationCallback(_callback: RecoveryConfirmationCallback): void {
    // TODO: 实现恢复确认逻辑
  }

  /** 获取当前阶段 */
  get phase(): OrchestratorPhase {
    return this.orchestratorAgent.state;
  }

  /** 获取当前执行计划 */
  get plan(): ExecutionPlan | null {
    return this.orchestratorAgent.context?.plan || null;
  }

  /** 是否正在运行（向后兼容） */
  get running(): boolean {
    return this.isRunning;
  }

  /** 中断当前任务（向后兼容） */
  async interrupt(): Promise<void> {
    await this.cancel();
  }


  /** 初始化编排者 */
  async initialize(): Promise<void> {
    await this.orchestratorAgent.initialize();

    if (this.config.verification) {
      this.verificationRunner = new VerificationRunner(
        this.workspaceRoot,
        this.config.verification
      );
    }
  }

  /**
   * 执行任务 - 主入口
   */
  async execute(userPrompt: string, taskId: string, sessionId?: string): Promise<string> {
    if (this.isRunning) {
      throw new Error('编排器正在运行中');
    }

    this.isRunning = true;
    this.currentTaskId = taskId;
    this.abortController = new AbortController();

    this.taskManager.updateTaskStatus(taskId, 'running');
    globalEventBus.emitEvent('task:started', { taskId, data: { isRunning: true } });
    this.startStatusUpdates(taskId);

    try {
      if (this.shouldUseAskMode(userPrompt)) {
        return await this.executeAskMode(userPrompt, taskId, sessionId);
      }

      // ask 模式：仅对话
      if (this.interactionMode === 'ask') {
        return await this.executeAskMode(userPrompt, taskId, sessionId);
      }

      // agent/auto 模式：使用独立编排者执行
      const result = await this.orchestratorAgent.execute(userPrompt, taskId, sessionId);

      if (this.abortController?.signal.aborted) {
        this.taskManager.updateTaskStatus(taskId, 'cancelled');
        globalEventBus.emitEvent('task:interrupted', { taskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      this.taskManager.updateTaskStatus(taskId, 'completed');
      globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });

      return result;

    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);

      if (this.abortController?.signal.aborted) {
        this.taskManager.updateTaskStatus(taskId, 'cancelled');
        globalEventBus.emitEvent('task:interrupted', { taskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      this.taskManager.updateTaskStatus(taskId, 'failed');
      globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
      throw error;

    } finally {
      this.isRunning = false;
      this.stopStatusUpdates();
      this.abortController = null;
      this.currentTaskId = null;
    }
  }

  /** ask 模式：仅对话 */
  private async executeAskMode(userPrompt: string, taskId: string, sessionId?: string): Promise<string> {
    console.log('[IntelligentOrchestrator] ask 模式：仅对话');

    const contextSessionId = sessionId || taskId;
    const context = await this.orchestratorAgent.prepareContext(contextSessionId, userPrompt);
    const prompt = context
      ? `请结合以下会话上下文回答用户问题。\n\n${context}\n\n## 用户问题\n${userPrompt}`
      : userPrompt;
    const snapshot = context ? this.truncateSnapshot(context) : undefined;

    const response = await this.cliFactory.sendMessage(
      'claude',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,
        adapterRole: 'orchestrator',
        messageMeta: {
          taskId,
          intent: 'ask',
          contextSnapshot: snapshot,
        },
      }
    );

    this.orchestratorAgent.recordOrchestratorTokens(response.tokenUsage);

    if (response.error) {
      throw new Error(response.error);
    }

    this.taskManager.updateTaskStatus(taskId, 'completed');
    globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });

    const content = response.content || '';
    await this.orchestratorAgent.recordAssistantMessage(content);
    return content;
  }

  private truncateSnapshot(context: string, maxChars: number = 6000): string {
    const trimmed = context.trim();
    if (trimmed.length <= maxChars) return trimmed;
    return trimmed.slice(0, maxChars) + '\n...';
  }

  private shouldUseAskMode(prompt: string): boolean {
    const trimmed = prompt.trim();
    if (!trimmed) return true;
    if (this.interactionMode === 'ask') return true;
    if (trimmed.startsWith('/agent') || trimmed.startsWith('/task')) return false;

    const lower = trimmed.toLowerCase();
    if (lower.includes('```') || /[\\/].+\.\w+/.test(lower)) return false;

    const hasTaskIntent = this.taskIntentKeywords.some(k => trimmed.includes(k));
    const hasBuildVerb = /(做|制作|搭建|实现)/.test(trimmed);
    const hasBuildTarget = /(功能|页面|模块|接口|系统|组件|服务|项目|API|后端|前端|UI|界面)/i.test(trimmed);
    const hasStructuredTaskIntent = hasTaskIntent || (hasBuildVerb && hasBuildTarget);
    if (hasStructuredTaskIntent) return false;

    const hasQuestion = trimmed.includes('?') || trimmed.includes('？');
    const hasDirectAnswerIntent = this.directAnswerKeywords.some(k => trimmed.includes(k));
    const shortPrompt = trimmed.length <= 50;

    return hasQuestion || hasDirectAnswerIntent || shortPrompt;
  }

  /** 取消当前任务 */
  async cancel(): Promise<void> {
    console.log('[IntelligentOrchestrator] 取消任务');

    // 1. 触发 AbortController
    this.abortController?.abort();

    // 2. 取消 OrchestratorAgent 中的任务
    await this.orchestratorAgent.cancel();

    // 3. 停止状态更新定时器
    this.stopStatusUpdates();

    // 4. 重置状态标志（关键！）
    this.isRunning = false;
    this.abortController = null;
    this.currentTaskId = null;

    console.log('[IntelligentOrchestrator] 任务已取消，状态已重置');
  }

  /** 开始状态更新定时器 */
  private startStatusUpdates(taskId: string): void {
    this.stopStatusUpdates();
    this.statusUpdateInterval = setInterval(() => {
      if (this.isRunning) {
        globalEventBus.emitEvent('orchestrator:phase_changed', {
          taskId,
          data: { phase: this.orchestratorAgent.state, isRunning: true },
        });
      }
    }, 2000);
  }

  /** 停止状态更新定时器 */
  private stopStatusUpdates(): void {
    if (this.statusUpdateInterval) {
      clearInterval(this.statusUpdateInterval);
      this.statusUpdateInterval = null;
    }
  }

  /** 获取可用的 CLI 列表 */
  getAvailableCLIs(): CLIType[] {
    return ['claude', 'codex', 'gemini'];
  }

  /** 🆕 获取执行统计摘要 */
  getStatsSummary(): string {
    return this.orchestratorAgent.getStatsSummary();
  }

  getOrchestratorTokenUsage(): { inputTokens: number; outputTokens: number } {
    return this.orchestratorAgent.getOrchestratorTokenUsage();
  }

  resetOrchestratorTokenUsage(): void {
    this.orchestratorAgent.resetOrchestratorTokenUsage();
  }

  /** 🆕 设置扩展上下文（用于持久化统计数据） */
  setExtensionContext(context: import('vscode').ExtensionContext): void {
    this.orchestratorAgent.setExtensionContext(context);
  }

  /** 🆕 获取执行统计实例（用于 UI 显示） */
  getExecutionStats(): import('./execution-stats').ExecutionStats | null {
    return this.orchestratorAgent.getExecutionStats();
  }

  /** 销毁编排器 */
  dispose(): void {
    this.stopStatusUpdates();
    this.orchestratorAgent.dispose();
    console.log('[IntelligentOrchestrator] 已销毁');
  }
}

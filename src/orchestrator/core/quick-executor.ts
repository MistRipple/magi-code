/**
 * Quick Executor - 快速执行路径
 *
 * 职责：
 * - 处理不需要完整 Mission 流程的简单请求
 * - 支持 ASK、DIRECT、EXPLORE 三种意图模式
 * - 直接选择 Worker 执行，无需创建 Mission
 * - 响应快速（目标 <500ms 启动）
 *
 * 设计原则：
 * - 不创建 Mission（节省创建/存储开销）
 * - 单 Worker 执行（不需要多 Worker 协作）
 * - 编排者视角输出（保持一致的用户体验）
 */

import { EventEmitter } from 'events';
import { WorkerSlot } from '../../types';
import { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../types/agent-types';
import { ProfileLoader, matchCategoryWithProfile } from '../profile';
import { LLMConfigLoader } from '../../llm/config';
import { logger, LogCategory } from '../../logging';
import { IntentHandlerMode } from '../intent-gate';

/**
 * 快速执行选项
 */
export interface QuickExecuteOptions {
  /** 用户请求 */
  prompt: string;
  /** 意图模式 */
  mode: IntentHandlerMode.ASK | IntentHandlerMode.DIRECT | IntentHandlerMode.EXPLORE;
  /** 工作目录 */
  workingDirectory: string;
  /** 会话 ID */
  sessionId?: string;
  /** 项目上下文 */
  projectContext?: string;
  /** 指定 Worker（可选，默认自动选择） */
  preferredWorker?: WorkerSlot;
  /** 超时时间(ms) */
  timeout?: number;
  /** 流式输出回调 */
  onStream?: (content: string) => void;
  /** 图片数据（可选） */
  imageData?: string[];
}

/**
 * 快速执行结果
 */
export interface QuickExecuteResult {
  /** 是否成功 */
  success: boolean;
  /** 响应内容 */
  content: string;
  /** 使用的 Worker */
  workerId: WorkerSlot;
  /** 执行模式 */
  mode: IntentHandlerMode;
  /** 执行耗时(ms) */
  duration: number;
  /** Token 使用 */
  tokenUsage?: TokenUsage;
  /** 错误信息 */
  error?: string;
}

/**
 * QuickExecutor - 快速执行器
 */
export class QuickExecutor extends EventEmitter {
  private adapterFactory: IAdapterFactory;
  private profileLoader: ProfileLoader;
  private workspaceRoot: string;

  constructor(
    adapterFactory: IAdapterFactory,
    profileLoader: ProfileLoader,
    workspaceRoot: string
  ) {
    super();
    this.adapterFactory = adapterFactory;
    this.profileLoader = profileLoader;
    this.workspaceRoot = workspaceRoot;
  }

  /**
   * 执行快速请求
   */
  async execute(options: QuickExecuteOptions): Promise<QuickExecuteResult> {
    const startTime = Date.now();

    logger.info('QuickExecutor.开始执行', {
      mode: options.mode,
      promptLength: options.prompt.length,
      preferredWorker: options.preferredWorker,
    }, LogCategory.ORCHESTRATOR);

    this.emit('executionStarted', {
      mode: options.mode,
      prompt: options.prompt.substring(0, 100),
    });

    try {
      // 1. 选择最佳 Worker
      const workerId = this.selectWorker(options);

      logger.debug('QuickExecutor.选择Worker', {
        workerId,
        mode: options.mode,
      }, LogCategory.ORCHESTRATOR);

      // 2. 构建执行 Prompt
      const executionPrompt = this.buildExecutionPrompt(options, workerId);

      // 3. 直接调用 Worker（通过 AdapterFactory）
      const response = await this.adapterFactory.sendMessage(
        workerId,
        executionPrompt,
        options.imageData,
        {
          source: 'orchestrator',
          streamToUI: true,
          adapterRole: 'worker',
          messageMeta: {
            sessionId: options.sessionId,
            mode: options.mode,
          },
        }
      );

      // 4. 处理响应
      const duration = Date.now() - startTime;

      if (response.error) {
        logger.warn('QuickExecutor.执行失败', {
          workerId,
          error: response.error,
          duration,
        }, LogCategory.ORCHESTRATOR);

        return {
          success: false,
          content: '',
          workerId,
          mode: options.mode,
          duration,
          error: response.error,
          tokenUsage: response.tokenUsage,
        };
      }

      logger.info('QuickExecutor.执行完成', {
        workerId,
        duration,
        contentLength: response.content?.length || 0,
      }, LogCategory.ORCHESTRATOR);

      this.emit('executionCompleted', {
        mode: options.mode,
        workerId,
        duration,
        success: true,
      });

      return {
        success: true,
        content: response.content || '',
        workerId,
        mode: options.mode,
        duration,
        tokenUsage: response.tokenUsage,
      };

    } catch (error) {
      const duration = Date.now() - startTime;
      const errorMessage = error instanceof Error ? error.message : String(error);

      logger.error('QuickExecutor.执行异常', {
        error: errorMessage,
        duration,
      }, LogCategory.ORCHESTRATOR);

      this.emit('executionFailed', {
        mode: options.mode,
        error: errorMessage,
        duration,
      });

      throw error instanceof Error ? error : new Error(errorMessage);
    }
  }

  /**
   * 选择最佳 Worker
   */
  private selectWorker(options: QuickExecuteOptions): WorkerSlot {
    // 如果用户指定了 Worker，优先使用（必须存在于画像配置）
    if (options.preferredWorker) {
      if (!this.profileLoader.getAllProfiles().has(options.preferredWorker)) {
        throw new Error(`指定 Worker 未配置: ${options.preferredWorker}`);
      }
      return options.preferredWorker;
    }

    const match = matchCategoryWithProfile(this.profileLoader, options.prompt);
    const preferred = match.categoryConfig?.defaultWorker;
    if (!preferred) {
      throw new Error(`任务分类未配置默认 Worker: ${match.category}`);
    }
    if (!this.profileLoader.getAllProfiles().has(preferred)) {
      throw new Error(`默认 Worker 未配置: ${preferred}`);
    }
    if (this.isWorkerAvailable(preferred)) {
      return preferred;
    }

    const fallback = this.selectFallbackWorker(match.category, preferred);
    if (!fallback) {
      throw new Error(`无可用 Worker（分类: ${match.category}）`);
    }
    logger.warn('QuickExecutor.Worker不可用_降级', {
      category: match.category,
      from: preferred,
      to: fallback,
    }, LogCategory.ORCHESTRATOR);
    return fallback;
  }

  private selectFallbackWorker(category: string, preferredWorker: WorkerSlot): WorkerSlot | null {
    const profiles = this.profileLoader.getAllProfiles();
    const connected = Array.from(profiles.keys()).filter(worker =>
      worker !== preferredWorker && this.isWorkerAvailable(worker)
    );

    if (connected.length === 0) {
      return null;
    }

    // 优先选择画像中偏好该分类的 Worker
    const preferredCandidates = connected.filter(worker => {
      const profile = profiles.get(worker);
      return Boolean(profile?.preferences.preferredCategories.includes(category));
    });

    if (preferredCandidates.length > 0) {
      return preferredCandidates[0];
    }

    return connected[0] ?? null;
  }

  private isWorkerAvailable(worker: WorkerSlot): boolean {
    if (this.adapterFactory.isConnected(worker)) {
      return true;
    }
    try {
      const workers = LLMConfigLoader.loadWorkersConfig();
      const cfg = workers[worker];
      return Boolean(cfg?.enabled && cfg.baseUrl && cfg.model);
    } catch {
      return false;
    }
  }

  /**
   * 构建执行 Prompt
   */
  private buildExecutionPrompt(options: QuickExecuteOptions, workerId: WorkerSlot): string {
    const sections: string[] = [];

    // 获取 Worker 画像
    const profile = this.profileLoader.getProfile(workerId);

    // 1. 角色指令
    sections.push(`# Role: ${profile?.displayName || workerId}`);
    sections.push(`You are handling a ${this.getModeDescription(options.mode)} request.`);

    // 2. 模式特定指令
    switch (options.mode) {
      case IntentHandlerMode.ASK:
        sections.push(`
## Instructions
- Provide a clear, helpful answer to the user's question
- Be concise but thorough
- If you need to reference code, include relevant snippets
- Do not perform any file modifications`);
        break;

      case IntentHandlerMode.EXPLORE:
        sections.push(`
## Instructions
- Analyze and explain the requested topic
- Provide insights and understanding
- Reference specific code or concepts as needed
- Focus on clarity and educational value`);
        break;

      case IntentHandlerMode.DIRECT:
        sections.push(`
## Instructions
- Execute the requested task directly
- Provide clear feedback on what was done
- If you create or modify files, report the changes
- Keep the response focused on the outcome`);
        break;
    }

    // 3. 项目上下文
    if (options.projectContext) {
      sections.push(`
## Project Context
${options.projectContext}`);
    }

    // 4. 工作目录
    sections.push(`
## Working Directory
${options.workingDirectory}`);

    // 5. 用户请求
    sections.push(`
## User Request
${options.prompt}`);

    return sections.join('\n\n');
  }

  /**
   * 获取模式描述
   */
  private getModeDescription(mode: IntentHandlerMode): string {
    switch (mode) {
      case IntentHandlerMode.ASK:
        return 'question/answer';
      case IntentHandlerMode.EXPLORE:
        return 'exploration/analysis';
      case IntentHandlerMode.DIRECT:
        return 'direct execution';
      default:
        return 'general';
    }
  }

  /**
   * 检查是否应该使用快速路径
   */
  static shouldUseQuickPath(mode: IntentHandlerMode): boolean {
    return mode === IntentHandlerMode.ASK ||
           mode === IntentHandlerMode.DIRECT ||
           mode === IntentHandlerMode.EXPLORE;
  }
}

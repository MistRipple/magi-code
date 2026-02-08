/**
 * Orchestrator Responder - 编排者响应器
 *
 * 职责：
 * - 接收 Worker 汇报
 * - 分析汇报内容
 * - 生成智能响应（继续/调整/终止/回答）
 *
 * 这是实现"智能编排"的关键组件：
 * - 让编排者能够根据 Worker 的执行情况动态调整策略
 * - 回答 Worker 的问题
 * - 在必要时终止执行
 */

import { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import { logger, LogCategory } from '../../../logging';
import { Mission, Assignment } from '../../mission';
import {
  WorkerReport,
  OrchestratorResponse,
  createContinueResponse,
  createAdjustResponse,
  createAbortResponse,
  createAnswerResponse,
} from '../../protocols/worker-report';

/**
 * 响应器选项
 */
export interface ResponderOptions {
  /** 使用 LLM 生成智能响应 */
  useLLM?: boolean;
  /** 最大失败次数（超过则终止） */
  maxFailures?: number;
  /** 最大问题次数（超过则终止） */
  maxQuestions?: number;
  /** Phase B+ 中间调用最小间隔（毫秒，默认 30000） */
  minReportInterval?: number;
}

/**
 * 响应器状态
 */
interface ResponderState {
  /** 累计失败次数 */
  failureCount: number;
  /** 累计问题次数 */
  questionCount: number;
  /** 历史汇报 */
  reportHistory: WorkerReport[];
  /** 每个 Worker 上次触发 LLM 调用的时间 */
  lastLLMCallTimestamps: Map<string, number>;
  /** 频率限制导致的排队汇报 */
  pendingReports: WorkerReport[];
}

/**
 * 编排者响应器
 */
export class OrchestratorResponder {
  private state: ResponderState = {
    failureCount: 0,
    questionCount: 0,
    reportHistory: [],
    lastLLMCallTimestamps: new Map(),
    pendingReports: [],
  };
  /** Phase B+ 中间调用最小间隔（毫秒） */
  private readonly minReportInterval: number;

  constructor(
    private adapterFactory: IAdapterFactory,
    private mission: Mission,
    private options: ResponderOptions = {}
  ) {
    this.options = {
      useLLM: true,
      maxFailures: 3,
      maxQuestions: 5,
      minReportInterval: 30_000,
      ...options,
    };
    this.minReportInterval = this.options.minReportInterval!;
  }

  /**
   * 处理 Worker 汇报并生成响应
   *
   * 频率限制策略：
   * - progress 类型：受频率限制，距上次 LLM 调用 < minReportInterval 时排队
   * - question / failed / completed：紧急类型，始终立即处理
   */
  async handleReport(report: WorkerReport): Promise<OrchestratorResponse> {
    this.state.reportHistory.push(report);

    logger.info(
      `收到 Worker 汇报: ${report.type}`,
      {
        workerId: report.workerId,
        assignmentId: report.assignmentId,
      },
      LogCategory.ORCHESTRATOR
    );

    switch (report.type) {
      case 'progress':
        // 频率限制：进度汇报受节流控制
        if (this.shouldThrottle(report.workerId)) {
          this.state.pendingReports.push(report);
          logger.debug('Phase B+ 频率限制：进度汇报已排队', {
            workerId: report.workerId,
            pendingCount: this.state.pendingReports.length,
          }, LogCategory.ORCHESTRATOR);
          return createContinueResponse();
        }
        return this.handleProgress(report);
      case 'question':
        return this.handleQuestion(report);
      case 'completed':
        return this.handleCompleted(report);
      case 'failed':
        return this.handleFailed(report);
      default:
        return createContinueResponse();
    }
  }

  /**
   * 处理进度汇报（通过频率限制后）
   */
  private async handleProgress(report: WorkerReport): Promise<OrchestratorResponse> {
    this.recordLLMCall(report.workerId);
    // 进度汇报通常直接继续，可在此添加动态调整逻辑（如偏离目标检测）
    return createContinueResponse();
  }

  /**
   * 处理问题汇报（紧急类型，不受频率限制）
   */
  private async handleQuestion(report: WorkerReport): Promise<OrchestratorResponse> {
    this.state.questionCount++;

    if (this.state.questionCount > (this.options.maxQuestions || 5)) {
      return createAbortResponse('问题次数过多，终止执行');
    }

    const question = report.question;
    if (!question) {
      return createContinueResponse();
    }

    // 使用 LLM 回答问题
    if (this.options.useLLM) {
      try {
        this.recordLLMCall(report.workerId);
        const answer = await this.generateAnswer(report);
        return createAnswerResponse(answer);
      } catch (error) {
        logger.warn(LogCategory.ORCHESTRATOR, 'LLM 回答问题失败，使用默认响应');
        return createContinueResponse();
      }
    }

    return createContinueResponse();
  }

  /**
   * 处理完成汇报（紧急类型）
   */
  private async handleCompleted(report: WorkerReport): Promise<OrchestratorResponse> {
    this.recordLLMCall(report.workerId);
    return createContinueResponse();
  }

  /**
   * 处理失败汇报（紧急类型）
   */
  private async handleFailed(report: WorkerReport): Promise<OrchestratorResponse> {
    this.state.failureCount++;

    if (this.state.failureCount > (this.options.maxFailures || 3)) {
      return createAbortResponse(`累计失败 ${this.state.failureCount} 次，终止执行`);
    }

    this.recordLLMCall(report.workerId);
    return createContinueResponse();
  }

  // ============================================================================
  // Phase B+ 频率限制
  // ============================================================================

  /**
   * 判断指定 Worker 是否应被节流
   */
  private shouldThrottle(workerId: string): boolean {
    const lastCall = this.state.lastLLMCallTimestamps.get(workerId);
    if (!lastCall) return false;
    return (Date.now() - lastCall) < this.minReportInterval;
  }

  /**
   * 记录 LLM 调用时间戳
   */
  private recordLLMCall(workerId: string): void {
    this.state.lastLLMCallTimestamps.set(workerId, Date.now());
  }

  /**
   * 处理排队中的汇报（由外部定时器或事件触发）
   *
   * 合并同一 Worker 的多个 progress 汇报为一个，避免 LLM 调用风暴。
   * 返回实际处理的汇报数量。
   */
  async processPendingReports(): Promise<number> {
    if (this.state.pendingReports.length === 0) return 0;

    // 按 workerId 分组，每个 Worker 只保留最新的一条
    const latestByWorker = new Map<string, WorkerReport>();
    for (const report of this.state.pendingReports) {
      latestByWorker.set(report.workerId, report);
    }

    // 清空排队
    this.state.pendingReports = [];

    let processedCount = 0;
    for (const [workerId, report] of latestByWorker) {
      if (!this.shouldThrottle(workerId)) {
        await this.handleProgress(report);
        processedCount++;
      } else {
        // 仍在冷却期，重新入队
        this.state.pendingReports.push(report);
      }
    }

    if (processedCount > 0) {
      logger.debug('Phase B+ 处理排队汇报', {
        processed: processedCount,
        remaining: this.state.pendingReports.length,
      }, LogCategory.ORCHESTRATOR);
    }

    return processedCount;
  }

  /**
   * 获取排队汇报数量
   */
  getPendingCount(): number {
    return this.state.pendingReports.length;
  }

  /**
   * 使用 LLM 生成回答
   */
  private async generateAnswer(report: WorkerReport): Promise<string> {
    const question = report.question!;
    const assignment = this.mission.assignments.find(a => a.id === report.assignmentId);

    const prompt = `你是一个智能编排者。Worker 在执行任务时遇到问题，需要你做决策。

## 任务目标
${this.mission.goal}

## 当前 Assignment
${assignment?.responsibility || '未知'}

## Worker 问题
${question.content}

${question.options?.length ? `## 可选项\n${question.options.map((o, i) => `${i + 1}. ${o}`).join('\n')}` : ''}

## 问题类型
${question.questionType}

请直接给出你的决策回答（简洁明了）：`;

    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      prompt,
      undefined,
      {
        adapterRole: 'orchestrator',
      }
    );

    return response.content;
  }

  /**
   * 分析执行情况，决定是否需要调整策略
   */
  async analyzeAndAdjust(): Promise<OrchestratorResponse | null> {
    // 如果有多次进度汇报但进展缓慢，可以调整策略
    const progressReports = this.state.reportHistory.filter(r => r.type === 'progress');

    if (progressReports.length >= 5) {
      // 检查是否进展缓慢（连续 5 次进度都在同一步骤）
      const lastFive = progressReports.slice(-5);
      const steps = lastFive.map(r => r.progress?.currentStep);
      const allSame = steps.every(s => s === steps[0]);

      if (allSame) {
        logger.warn(LogCategory.ORCHESTRATOR, '检测到执行停滞，建议调整策略');
        return createAdjustResponse({
          newInstructions: '执行似乎停滞，请尝试其他方法或简化步骤',
        });
      }
    }

    return null;
  }

  /**
   * 重置状态
   */
  reset(): void {
    this.state = {
      failureCount: 0,
      questionCount: 0,
      reportHistory: [],
      lastLLMCallTimestamps: new Map(),
      pendingReports: [],
    };
  }
}

/**
 * Intent Gate - 意图门控（AI 决策版）
 *
 * 定位：Layer 3（plan_mission 路径）的前置意图分类器。
 * 在 MissionOrchestrator.processRequest() 中被调用，用于决定处理模式：
 *   - ASK/DIRECT/EXPLORE → 跳过 Mission 创建，直接响应
 *   - TASK/DEMO → 进入完整 Mission 流程
 *   - CLARIFY → 向用户提问后重新分类
 *
 * 注意：Layer 2（dispatch_task 路径）不经过此组件，
 * 其意图判断融入 buildUnifiedSystemPrompt 系统提示词中，
 * 由 orchestrator LLM 自主通过工具选择表达决策。
 *
 * 意图判定完全由 AI 决策。
 */

/** AI 意图决策 */
export interface IntentDecision {
  /** 意图类型（用于分析/记录） */
  intent: 'question' | 'trivial' | 'exploratory' | 'task' | 'demo' | 'ambiguous' | 'open_ended';
  /** 推荐的处理模式 */
  recommendedMode: IntentHandlerMode;
  /** 置信度 (0-1) */
  confidence: number;
  /** 是否需要澄清 */
  needsClarification: boolean;
  /** 澄清问题 */
  clarificationQuestions?: string[];
  /** 决策理由 */
  reason: string;
}

/** 意图处理结果 */
export interface IntentGateResult {
  /** 意图分类 */
  classification: IntentDecision;
  /** 推荐的处理模式 */
  recommendedMode: IntentHandlerMode;
  /** 是否应该跳过任务分析 */
  skipTaskAnalysis: boolean;
  /** 是否需要用户澄清 */
  needsClarification: boolean;
  /** 澄清问题 */
  clarificationQuestions?: string[];
  /** 处理建议 */
  suggestion: string;
}

/** 意图处理模式 */
export enum IntentHandlerMode {
  /** 直接回答模式：问答、咨询、问候 */
  ASK = 'ask',
  /** 直接执行模式：简单操作，无需计划 */
  DIRECT = 'direct',
  /** 探索模式：代码分析、理解 */
  EXPLORE = 'explore',
  /** 任务模式：需要计划和执行 */
  TASK = 'task',
  /** 演示模式：测试/演示系统功能，Orchestrator 自主选择场景 */
  DEMO = 'demo',
  /** 澄清模式：需要用户提供更多信息 */
  CLARIFY = 'clarify',
}

export type IntentDecider = (prompt: string) => Promise<IntentDecision>;

/**
 * 意图门控
 * 
 * 核心职责：
 * 1. 对用户输入进行意图分类
 * 2. 根据意图类型推荐处理模式
 * 3. 判断是否需要用户澄清
 */
export class IntentGate {
  private decider: IntentDecider;

  constructor(decider: IntentDecider) {
    this.decider = decider;
  }

  /**
   * 处理用户输入，返回意图门控结果
   */
  async process(prompt: string): Promise<IntentGateResult> {
    // 1. 意图分类（AI 决策）
    const classification = await this.decider(prompt);

    // 2. 确定处理模式（由 AI 决策输出）
    const recommendedMode = classification.recommendedMode;

    // 3. 判断是否跳过任务分析
    const skipTaskAnalysis = this.shouldSkipTaskAnalysis(recommendedMode);

    // 4. 判断是否需要澄清
    const questions = Array.isArray(classification.clarificationQuestions)
      ? classification.clarificationQuestions.filter(q => typeof q === 'string' && q.trim())
      : [];
    const needsClarification = Boolean(classification.needsClarification || recommendedMode === IntentHandlerMode.CLARIFY) &&
      questions.length > 0;

    // 5. 生成处理建议
    const suggestion = this.generateSuggestion(classification, recommendedMode);

    return {
      classification,
      recommendedMode,
      skipTaskAnalysis,
      needsClarification,
      clarificationQuestions: questions,
      suggestion,
    };
  }

  /**
   * 判断是否应该跳过任务分析
   */
  private shouldSkipTaskAnalysis(
    mode: IntentHandlerMode
  ): boolean {
    // 问答、直接执行、探索、澄清模式都跳过任务分析
    return [
      IntentHandlerMode.ASK,
      IntentHandlerMode.DIRECT,
      IntentHandlerMode.EXPLORE,
      IntentHandlerMode.CLARIFY,
    ].includes(mode);
  }

  /**
   * 生成处理建议
   */
  private generateSuggestion(
    classification: IntentDecision,
    mode: IntentHandlerMode
  ): string {
    const modeDescriptions: Record<IntentHandlerMode, string> = {
      [IntentHandlerMode.ASK]: '直接回答用户问题',
      [IntentHandlerMode.DIRECT]: '直接执行简单操作',
      [IntentHandlerMode.EXPLORE]: '探索分析代码库',
      [IntentHandlerMode.TASK]: '进入任务分析和执行流程',
      [IntentHandlerMode.DEMO]: '自主选择测试场景并执行',
      [IntentHandlerMode.CLARIFY]: '请求用户提供更多信息',
    };

    return `意图类型: ${classification.intent}, 置信度: ${(classification.confidence * 100).toFixed(0)}%, ` +
           `建议: ${modeDescriptions[mode]}`;
  }
}

/**
 * Intent Gate - 意图门控
 * 
 * 作为编排器的入口层，在处理任何消息前先进行意图分类和路由。
 * 参考 Oh-My-OpenCode 的 Phase 0 - Intent Gate 模式。
 */

import { IntentClassifier, IntentType, IntentClassification } from './intent-classifier';

/** 意图处理结果 */
export interface IntentGateResult {
  /** 意图分类 */
  classification: IntentClassification;
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
  /** 澄清模式：需要用户提供更多信息 */
  CLARIFY = 'clarify',
}

/** Intent Gate 配置 */
export interface IntentGateConfig {
  /** 是否启用澄清机制 */
  enableClarification: boolean;
  /** 低置信度阈值（低于此值触发澄清） */
  lowConfidenceThreshold: number;
}

const DEFAULT_CONFIG: IntentGateConfig = {
  enableClarification: true,
  lowConfidenceThreshold: 0.6,
};

/**
 * 意图门控
 * 
 * 核心职责：
 * 1. 对用户输入进行意图分类
 * 2. 根据意图类型推荐处理模式
 * 3. 判断是否需要用户澄清
 */
export class IntentGate {
  private classifier: IntentClassifier;
  private config: IntentGateConfig;

  constructor(config: Partial<IntentGateConfig> = {}) {
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.classifier = new IntentClassifier();
  }

  /**
   * 处理用户输入，返回意图门控结果
   */
  process(prompt: string): IntentGateResult {
    // 1. 意图分类
    const classification = this.classifier.classify(prompt);
    
    // 2. 确定处理模式
    const recommendedMode = this.determineHandlerMode(classification);
    
    // 3. 判断是否跳过任务分析
    const skipTaskAnalysis = this.shouldSkipTaskAnalysis(classification, recommendedMode);
    
    // 4. 判断是否需要澄清
    const needsClarification = this.shouldRequestClarification(classification);
    
    // 5. 生成处理建议
    const suggestion = this.generateSuggestion(classification, recommendedMode);

    return {
      classification,
      recommendedMode,
      skipTaskAnalysis,
      needsClarification,
      clarificationQuestions: classification.clarificationQuestions,
      suggestion,
    };
  }

  /**
   * 根据意图分类确定处理模式
   */
  private determineHandlerMode(classification: IntentClassification): IntentHandlerMode {
    // 如果需要澄清且启用了澄清机制
    if (classification.needsClarification && this.config.enableClarification) {
      return IntentHandlerMode.CLARIFY;
    }

    switch (classification.type) {
      case IntentType.QUESTION:
        return IntentHandlerMode.ASK;
      
      case IntentType.TRIVIAL:
        return IntentHandlerMode.DIRECT;
      
      case IntentType.EXPLORATORY:
        return IntentHandlerMode.EXPLORE;
      
      case IntentType.EXPLICIT:
      case IntentType.OPEN_ENDED:
        return IntentHandlerMode.TASK;
      
      case IntentType.AMBIGUOUS:
        return this.config.enableClarification 
          ? IntentHandlerMode.CLARIFY 
          : IntentHandlerMode.ASK; // 降级为问答
      
      default:
        return IntentHandlerMode.ASK;
    }
  }

  /**
   * 判断是否应该跳过任务分析
   */
  private shouldSkipTaskAnalysis(
    classification: IntentClassification, 
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
   * 判断是否需要请求用户澄清
   */
  private shouldRequestClarification(classification: IntentClassification): boolean {
    if (!this.config.enableClarification) {
      return false;
    }
    
    // 明确标记需要澄清
    if (classification.needsClarification) {
      return true;
    }
    
    // 置信度过低
    if (classification.confidence < this.config.lowConfidenceThreshold) {
      return true;
    }
    
    return false;
  }

  /**
   * 生成处理建议
   */
  private generateSuggestion(
    classification: IntentClassification, 
    mode: IntentHandlerMode
  ): string {
    const modeDescriptions: Record<IntentHandlerMode, string> = {
      [IntentHandlerMode.ASK]: '直接回答用户问题',
      [IntentHandlerMode.DIRECT]: '直接执行简单操作',
      [IntentHandlerMode.EXPLORE]: '探索分析代码库',
      [IntentHandlerMode.TASK]: '进入任务分析和执行流程',
      [IntentHandlerMode.CLARIFY]: '请求用户提供更多信息',
    };

    return `意图类型: ${classification.type}, 置信度: ${(classification.confidence * 100).toFixed(0)}%, ` +
           `建议: ${modeDescriptions[mode]}`;
  }
}

export { IntentClassifier, IntentType, IntentClassification };


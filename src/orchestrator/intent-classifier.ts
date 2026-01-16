/**
 * Intent Classifier - 意图分类器
 * 
 * 基于 Oh-My-OpenCode 的 Intent Gate 模式，对用户输入进行意图分类。
 * 核心原则：NEVER START IMPLEMENTING, UNLESS USER WANTS YOU TO IMPLEMENT SOMETHING EXPLICITLY
 */

/** 意图类型 */
export enum IntentType {
  /** 问答咨询：解释、建议、概念、问候 */
  QUESTION = 'question',
  /** 简单操作：单文件、已知位置、简单修改 */
  TRIVIAL = 'trivial',
  /** 明确任务：具体文件、明确命令、代码块 */
  EXPLICIT = 'explicit',
  /** 探索分析：代码理解、模式查找、架构分析 */
  EXPLORATORY = 'exploratory',
  /** 开放需求：功能开发、重构、改进 */
  OPEN_ENDED = 'open_ended',
  /** 模糊请求：范围不清、多种解释、缺少关键信息 */
  AMBIGUOUS = 'ambiguous',
}

/** 意图分类结果 */
export interface IntentClassification {
  /** 意图类型 */
  type: IntentType;
  /** 置信度 (0-1) */
  confidence: number;
  /** 分类原因 */
  reason: string;
  /** 是否需要澄清 */
  needsClarification: boolean;
  /** 澄清问题（如果需要） */
  clarificationQuestions?: string[];
  /** 检测到的信号 */
  signals: {
    hasQuestionMark: boolean;
    hasQuestionKeyword: boolean;
    hasTaskKeyword: boolean;
    hasCodeBlock: boolean;
    hasFilePath: boolean;
    isShortText: boolean;
    hasExploratoryKeyword: boolean;
    hasOpenEndedKeyword: boolean;
  };
}

/** 意图分类器配置 */
export interface IntentClassifierConfig {
  /** 短文本阈值（字符数） */
  shortTextThreshold: number;
  /** 高置信度阈值 */
  highConfidenceThreshold: number;
  /** 是否启用 LLM 辅助分类 */
  enableLLMAssist: boolean;
}

const DEFAULT_CONFIG: IntentClassifierConfig = {
  shortTextThreshold: 30,
  highConfidenceThreshold: 0.8,
  enableLLMAssist: false,
};

/**
 * 意图分类器
 */
export class IntentClassifier {
  private config: IntentClassifierConfig;

  /** 问答关键词 */
  private readonly questionKeywords = [
    '是什么', '什么是', '为什么', '怎么', '如何', '能否', '可以吗', '建议', '解释', '说明',
    '介绍', '告诉我', '帮我理解', '对比', '优缺点', '方案', '思路', '总结', '概念', '原理',
    '问题', '是否', '推荐', '区别', '差异', '哪个', '什么时候', '为何', '能不能',
    'what', 'why', 'how', 'when', 'which', 'explain', 'describe', 'compare', 'difference',
    '你可以', '你能', '你会', '你是', '你好', 'hello', 'hi',
  ];

  /** 任务执行关键词 */
  private readonly taskKeywords = [
    '实现', '添加', '新增', '修改', '修复', '重构', '迁移', '集成', '优化', '部署', '测试',
    '生成', '创建', '删除', '更新', '写', '改', '开发', '搭建', '编排', '完善', '构建',
    '做一个', '帮我做', '帮我写', '帮我改', '帮我修', '帮我创建', '帮我实现', '帮我添加',
    'implement', 'add', 'create', 'fix', 'refactor', 'build', 'develop', 'update', 'delete',
  ];

  /** 探索分析关键词 */
  private readonly exploratoryKeywords = [
    '分析', '查找', '找到', '搜索', '理解', '了解', '看看', '检查', '审查', '评估',
    '怎么工作', '如何运作', '代码结构', '架构', '依赖', '调用关系',
    'analyze', 'find', 'search', 'understand', 'review', 'check', 'evaluate',
  ];

  /** 开放需求关键词 */
  private readonly openEndedKeywords = [
    '改进', '优化', '提升', '增强', '完善', '升级', '扩展',
    '功能', '特性', '模块', '系统', '服务', '组件',
    'improve', 'enhance', 'upgrade', 'extend', 'feature', 'module',
  ];

  constructor(config: Partial<IntentClassifierConfig> = {}) {
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 分类用户意图
   */
  classify(prompt: string): IntentClassification {
    const trimmed = prompt.trim();
    const signals = this.detectSignals(trimmed);
    
    // 规则分类
    const result = this.classifyByRules(trimmed, signals);
    
    return result;
  }

  /**
   * 检测输入信号
   */
  private detectSignals(prompt: string): IntentClassification['signals'] {
    const lower = prompt.toLowerCase();
    
    return {
      hasQuestionMark: prompt.includes('?') || prompt.includes('？'),
      hasQuestionKeyword: this.questionKeywords.some(k => prompt.includes(k) || lower.includes(k.toLowerCase())),
      hasTaskKeyword: this.taskKeywords.some(k => prompt.includes(k) || lower.includes(k.toLowerCase())),
      hasCodeBlock: prompt.includes('```'),
      hasFilePath: /[\\/][\w\-./]+\.\w+/.test(prompt) || /\w+\.\w{2,4}/.test(prompt),
      isShortText: prompt.length <= this.config.shortTextThreshold,
      hasExploratoryKeyword: this.exploratoryKeywords.some(k => prompt.includes(k) || lower.includes(k.toLowerCase())),
      hasOpenEndedKeyword: this.openEndedKeywords.some(k => prompt.includes(k) || lower.includes(k.toLowerCase())),
    };
  }

  /**
   * 基于规则的意图分类
   */
  private classifyByRules(prompt: string, signals: IntentClassification['signals']): IntentClassification {
    const base = { signals, needsClarification: false };

    // 1. 有代码块或明确文件路径 → 明确任务（最高优先级）
    if (signals.hasCodeBlock || (signals.hasFilePath && signals.hasTaskKeyword)) {
      return {
        ...base,
        type: IntentType.EXPLICIT,
        confidence: 0.9,
        reason: '包含代码块或明确的文件路径和任务关键词',
      };
    }

    // 2. 探索分析类（优先于问答判断）
    if (signals.hasExploratoryKeyword && !signals.hasTaskKeyword) {
      return {
        ...base,
        type: IntentType.EXPLORATORY,
        confidence: 0.85,
        reason: '检测到探索分析关键词，无明确任务意图',
      };
    }

    // 3. 明显的问答类
    if (this.isObviousQuestion(signals)) {
      return {
        ...base,
        type: IntentType.QUESTION,
        confidence: 0.95,
        reason: '检测到问答特征：问号或问答关键词，且无任务关键词',
      };
    }

    // 4. 短文本且无任务关键词且无探索关键词 → 问答
    if (signals.isShortText && !signals.hasTaskKeyword && !signals.hasCodeBlock && !signals.hasFilePath && !signals.hasExploratoryKeyword) {
      return {
        ...base,
        type: IntentType.QUESTION,
        confidence: 0.85,
        reason: '短文本且无任务特征，视为问答或问候',
      };
    }

    // 5. 开放需求类（有任务词 + 开放词，但无具体文件）
    if (signals.hasOpenEndedKeyword && signals.hasTaskKeyword && !signals.hasFilePath) {
      return {
        ...base,
        type: IntentType.OPEN_ENDED,
        confidence: 0.8,
        reason: '检测到开放需求特征：有任务意图但范围较广',
      };
    }

    // 6. 有明确任务关键词 → 明确任务（不再检查缺失信息，让任务分析阶段处理）
    if (signals.hasTaskKeyword) {
      return {
        ...base,
        type: IntentType.EXPLICIT,
        confidence: 0.85,
        reason: '检测到明确的任务关键词',
      };
    }

    // 7. 默认：模糊请求
    return {
      ...base,
      type: IntentType.AMBIGUOUS,
      confidence: 0.5,
      reason: '无法明确判断意图类型',
      needsClarification: true,
      clarificationQuestions: ['请问您具体想要完成什么任务？'],
    };
  }

  /**
   * 判断是否为明显的问答类
   */
  private isObviousQuestion(signals: IntentClassification['signals']): boolean {
    // 有问号且无任务关键词
    if (signals.hasQuestionMark && !signals.hasTaskKeyword) {
      return true;
    }
    // 有问答关键词且无任务关键词
    if (signals.hasQuestionKeyword && !signals.hasTaskKeyword) {
      return true;
    }
    return false;
  }

  /**
   * 检查缺少的关键信息
   */
  private checkMissingInfo(prompt: string, signals: IntentClassification['signals']): string[] {
    const questions: string[] = [];
    
    // 如果提到修复但没有具体错误信息
    if (/修复|fix|bug/i.test(prompt) && !signals.hasFilePath && !/错误|error|问题/i.test(prompt)) {
      questions.push('请提供具体的错误信息或问题描述');
    }
    
    // 如果是功能开发但范围不清
    if (/功能|feature/i.test(prompt) && prompt.length < 50) {
      questions.push('请详细描述功能需求和预期行为');
    }
    
    return questions;
  }
}


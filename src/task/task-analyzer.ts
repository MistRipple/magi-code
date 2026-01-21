// TASK ANALYZER TEST
/**
 * 任务分析器
 * 解析用户输入，识别任务类型、复杂度和目标文件
 *
 * 支持画像系统的分类配置扩展
 */

import { TaskCategory, CLIType } from '../types';
import { ProfileLoader, CategoryConfig, RiskLevel } from '../orchestrator/profile';
import { DEFAULT_CATEGORIES_CONFIG } from '../orchestrator/profile/defaults';

/** 任务分析结果 */
export interface TaskAnalysis {
  /** 任务类型 */
  category: TaskCategory;
  /** 复杂度评分 (1-5) */
  complexity: number;
  /** 识别的目标文件 */
  targetFiles: string[];
  /** 识别的关键词 */
  keywords: string[];
  /** 是否可拆分 */
  splittable: boolean;
  /** 建议的执行模式 */
  suggestedMode: 'sequential' | 'parallel';
  /** 原始 prompt */
  prompt: string;
  /** 是否为问答/咨询类请求（不需要执行任务） */
  isQuestion: boolean;
  /** 风险等级（来自画像系统） */
  riskLevel?: RiskLevel;
  /** 推荐的 Worker（来自画像系统） */
  recommendedWorker?: CLIType;
  /** 用户显式指定的 Worker */
  explicitWorkers?: CLIType[];
  /** 用户是否明确要求并行 */
  wantsParallel?: boolean;
  /** 匹配的关键词 */
  matchedKeywords?: string[];
}

/** 任务类型关键词映射 */
const CATEGORY_KEYWORDS: Record<TaskCategory, string[]> = {
  architecture: ['架构', '设计', '重构', 'architecture', 'design', 'refactor', '模块', '结构'],
  implement: ['实现', '添加', '创建', '开发', 'implement', 'add', 'create', 'develop', '功能'],
  refactor: ['重构', '优化', '改进', 'refactor', 'optimize', 'improve', '性能'],
  bugfix: ['修复', 'bug', 'fix', '错误', '问题', 'error', 'issue', '调试', 'debug'],
  debug: ['调试', 'debug', '排查', '分析', 'trace', '日志'],
  frontend: ['前端', 'frontend', 'ui', 'css', '样式', '组件', 'component', 'react', 'vue'],
  backend: ['后端', 'backend', 'api', '服务', 'server', '数据库', 'database'],
  test: ['测试', 'test', '单元测试', 'unit', '覆盖率', 'coverage', 'jest', 'mocha'],
  document: ['文档', '注释', 'doc', 'comment', 'readme', '说明'],
  review: ['审查', 'review', '检查', 'check', '代码审查'],
  simple: ['简单', '小', '快速', 'simple', 'small', 'quick', '单个'],
  general: [],
};

/** 复杂度关键词 */
const COMPLEXITY_INDICATORS = {
  high: ['全部', '所有', '整个', '重构', '架构', 'all', 'entire', 'complete', '多个文件'],
  medium: ['添加', '修改', '更新', 'add', 'modify', 'update', '几个'],
  low: ['简单', '小', '快速', 'simple', 'small', 'quick', '单个'],
};

/**
 * 任务分析器类
 * 支持画像系统的分类配置扩展
 */
export class TaskAnalyzer {
  /** 画像加载器（可选） */
  private profileLoader?: ProfileLoader;

  /** 问答/咨询类关键词 */
  private readonly questionKeywords = [
    '是什么', '什么是', '为什么', '怎么', '如何', '能否', '可以吗', '建议', '解释', '说明',
    '介绍', '告诉我', '帮我理解', '对比', '优缺点', '方案', '思路', '总结', '概念', '原理',
    '问题', '是否', '推荐', '区别', '差异', '哪个', '什么时候', '为何', '能不能',
    'what', 'why', 'how', 'when', 'which', 'explain', 'describe', 'compare', 'difference',
    '你可以', '你能', '你会', '你是'
  ];

  /** 任务执行类关键词 */
  private readonly taskKeywords = [
    '实现', '添加', '新增', '修改', '修复', '重构', '迁移', '集成', '优化', '部署', '测试',
    '生成', '创建', '删除', '更新', '写', '改', '开发', '搭建', '编排', '完善', '构建',
    '做一个', '帮我做', '帮我写', '帮我改', '帮我修', '帮我创建', '帮我实现', '帮我添加'
  ];

  /**
   * 设置画像加载器
   */
  setProfileLoader(loader: ProfileLoader): void {
    this.profileLoader = loader;
  }

  /**
   * 分析用户输入
   * 集成画像系统的分类配置
   */
  analyze(prompt: string): TaskAnalysis {
    const lowerPrompt = prompt.toLowerCase();

    // 首先判断是否为问答类请求
    const isQuestion = this.detectIsQuestion(prompt);

    // 识别任务类型（优先使用画像系统）
    const { category, matchedKeywords, categoryConfig } = this.detectCategoryWithProfile(lowerPrompt);

    // 识别目标文件
    const targetFiles = this.extractTargetFiles(prompt);

    // 识别关键词
    const keywords = this.extractKeywords(lowerPrompt);

    // 评估复杂度
    const complexity = this.assessComplexity(prompt, targetFiles, keywords);

    // 判断是否可拆分
    const splittable = this.isSplittable(prompt, category, complexity);

    // 建议执行模式
    const suggestedMode = this.suggestMode(targetFiles, splittable);

    // 显式指定 Worker 与并行意图
    const explicitWorkers = this.detectExplicitWorkers(lowerPrompt);
    const wantsParallel = this.detectWantsParallel(lowerPrompt);

    // 从画像配置获取风险等级和推荐 Worker
    const riskLevel = categoryConfig?.riskLevel;
    const recommendedWorker = categoryConfig?.defaultWorker as CLIType | undefined;

    return {
      category,
      complexity,
      targetFiles,
      keywords,
      splittable,
      suggestedMode,
      prompt,
      isQuestion,
      riskLevel,
      recommendedWorker,
      matchedKeywords,
      explicitWorkers,
      wantsParallel,
    };
  }

  /**
   * 使用画像系统检测分类
   */
  private detectCategoryWithProfile(lowerPrompt: string): {
    category: TaskCategory;
    matchedKeywords: string[];
    categoryConfig?: CategoryConfig;
  } {
    const categories = this.profileLoader?.getAllCategories();
    const rules = this.profileLoader?.getCategoryRules() ?? DEFAULT_CATEGORIES_CONFIG.rules;

    let bestMatch: { category: string; score: number; keywords: string[]; config: CategoryConfig } | null = null;

    for (const categoryName of rules.categoryPriority) {
      const config = categories?.get(categoryName) || DEFAULT_CATEGORIES_CONFIG.categories[categoryName];
      if (!config) continue;

      let score = 0;
      const matched: string[] = [];

      for (const pattern of config.keywords) {
        try {
          const regex = new RegExp(pattern, 'i');
          if (regex.test(lowerPrompt)) {
            score += 10;
            matched.push(pattern);
          }
        } catch {
          if (lowerPrompt.includes(pattern.toLowerCase())) {
            score += 5;
            matched.push(pattern);
          }
        }
      }

      if (score > 0 && (!bestMatch || score > bestMatch.score)) {
        bestMatch = { category: categoryName, score, keywords: matched, config };
      }
    }

    if (bestMatch) {
      return {
        category: bestMatch.category as TaskCategory,
        matchedKeywords: bestMatch.keywords,
        categoryConfig: bestMatch.config,
      };
    }

    const defaultCategory = rules.defaultCategory as TaskCategory;
    const defaultConfig = categories?.get(defaultCategory) || DEFAULT_CATEGORIES_CONFIG.categories[defaultCategory];
    return { category: defaultCategory, matchedKeywords: [], categoryConfig: defaultConfig };
  }

  /**
   * 检测是否为问答/咨询类请求
   */
  private detectIsQuestion(prompt: string): boolean {
    const trimmed = prompt.trim();
    const hasBuildVerb = /(做|制作|搭建|实现|开发|修复|重构|新增|优化|编写|添加|修改)/.test(trimmed);
    const hasBuildTarget = /(功能|页面|模块|接口|系统|组件|服务|项目|API|后端|前端|UI|界面)/i.test(trimmed);
    const capabilityPattern = /(你能|你可以|你会|能不能|能否|是否|可以|支持)/;
    const endsWithQuestionWord = /(吗|么|？|\?)$/.test(trimmed);
    const hasCapabilityQuestion = capabilityPattern.test(trimmed)
      && (endsWithQuestionWord || /(能做|能否做|可以做)/.test(trimmed))
      && !hasBuildTarget
      && !/(代码|文件|改动|实现|开发|修复|重构|新增|优化)/.test(trimmed);

    // 1. 包含问号
    if (trimmed.includes('?') || trimmed.includes('？')) {
      // 但如果同时包含任务关键词，则不是纯问答
      const hasTaskKeyword = this.taskKeywords.some(k => trimmed.includes(k));
      if (!hasTaskKeyword || hasCapabilityQuestion) return true;
    }

    // 2. 包含问答关键词
    const hasQuestionKeyword = this.questionKeywords.some(k => trimmed.includes(k));
    const hasTaskKeyword = this.taskKeywords.some(k => trimmed.includes(k));

    // 有问答关键词且没有任务关键词
    if (hasQuestionKeyword && (!hasTaskKeyword || hasCapabilityQuestion)) return true;

    // 3. 短文本且没有任务关键词（可能是简单问候或询问）
    if (trimmed.length <= 30 && !hasTaskKeyword) {
      // 检查是否包含代码块或文件路径
      if (trimmed.includes('```') || /[\\/].+\.\w+/.test(trimmed)) {
        return false;
      }
      return true;
    }

    // 4. 能力/可行性询问（即便包含任务关键词）
    if (hasCapabilityQuestion && !(hasBuildVerb && hasBuildTarget)) {
      return true;
    }

    return false;
  }

  /**
   * 检测任务类型
   */
  private detectCategory(prompt: string): TaskCategory {
    let maxScore = 0;
    let detectedCategory: TaskCategory = 'general';

    for (const [category, keywords] of Object.entries(CATEGORY_KEYWORDS)) {
      const score = keywords.filter(k => prompt.includes(k)).length;
      if (score > maxScore) {
        maxScore = score;
        detectedCategory = category as TaskCategory;
      }
    }

    return detectedCategory;
  }

  /**
   * 提取目标文件
   */
  private extractTargetFiles(prompt: string): string[] {
    // 匹配文件路径模式
    const filePattern = /[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|scss|html|json|md|yaml|yml)/gi;
    const matches = prompt.match(filePattern);
    return matches ? [...new Set(matches)] : [];
  }

  /**
   * 提取关键词
   */
  private extractKeywords(prompt: string): string[] {
    const allKeywords = Object.values(CATEGORY_KEYWORDS).flat();
    return allKeywords.filter(k => prompt.includes(k));
  }

  /**
   * 评估复杂度 (1-5)
   */
  private assessComplexity(prompt: string, files: string[], keywords: string[]): number {
    let score = 2; // 基础分

    // 文件数量影响
    if (files.length > 5) score += 2;
    else if (files.length > 2) score += 1;

    // 高复杂度关键词
    if (COMPLEXITY_INDICATORS.high.some(k => prompt.toLowerCase().includes(k))) score += 1;
    
    // 低复杂度关键词
    if (COMPLEXITY_INDICATORS.low.some(k => prompt.toLowerCase().includes(k))) score -= 1;

    // prompt 长度影响
    if (prompt.length > 200) score += 1;

    return Math.max(1, Math.min(5, score));
  }

  /**
   * 判断是否可拆分
   */
  private isSplittable(prompt: string, category: TaskCategory, complexity: number): boolean {
    // 复杂度高的任务通常可拆分
    if (complexity >= 4) return true;
    
    // 包含多个动作的任务可拆分
    const actionWords = ['和', '并且', '同时', 'and', 'also', '以及', '还要'];
    if (actionWords.some(w => prompt.includes(w))) return true;

    // 架构类任务通常可拆分
    if (category === 'architecture') return true;

    return false;
  }

  /**
   * 建议执行模式
   */
  private suggestMode(files: string[], splittable: boolean): 'sequential' | 'parallel' {
    // 如果目标文件不重叠，可以并行
    if (splittable && files.length > 1) {
      return 'parallel';
    }
    return 'sequential';
  }

  private detectExplicitWorkers(promptLower: string): CLIType[] {
    const workers: CLIType[] = [];
    if (promptLower.includes('claude')) workers.push('claude');
    if (promptLower.includes('codex')) workers.push('codex');
    if (promptLower.includes('gemini')) workers.push('gemini');
    return workers;
  }

  private detectWantsParallel(promptLower: string): boolean {
    const parallelKeywords = [
      '并行',
      '并发',
      '同时',
      'parallel',
      'concurrent',
    ];
    return parallelKeywords.some(k => promptLower.includes(k));
  }
}

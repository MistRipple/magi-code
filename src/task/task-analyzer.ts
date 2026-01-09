/**
 * 任务分析器
 * 解析用户输入，识别任务类型、复杂度和目标文件
 */

import { TaskCategory, CLIType } from '../types';

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
}

/** 任务类型关键词映射 */
const CATEGORY_KEYWORDS: Record<TaskCategory, string[]> = {
  architecture: ['架构', '设计', '重构', 'architecture', 'design', 'refactor', '模块', '结构'],
  implement: ['实现', '添加', '创建', '开发', 'implement', 'add', 'create', 'develop', '功能'],
  refactor: ['重构', '优化', '改进', 'refactor', 'optimize', 'improve', '性能'],
  bugfix: ['修复', 'bug', 'fix', '错误', '问题', 'error', 'issue', '调试', 'debug'],
  debug: ['调试', 'debug', '排查', '分析', 'trace', '日志'],
  frontend: ['前端', 'frontend', 'ui', 'css', '样式', '组件', 'component', 'react', 'vue'],
  test: ['测试', 'test', '单元测试', 'unit', '覆盖率', 'coverage', 'jest', 'mocha'],
  document: ['文档', '注释', 'doc', 'comment', 'readme', '说明'],
  review: ['审查', 'review', '检查', 'check', '代码审查'],
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
 */
export class TaskAnalyzer {
  /**
   * 分析用户输入
   */
  analyze(prompt: string): TaskAnalysis {
    const lowerPrompt = prompt.toLowerCase();
    
    // 识别任务类型
    const category = this.detectCategory(lowerPrompt);
    
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

    return {
      category,
      complexity,
      targetFiles,
      keywords,
      splittable,
      suggestedMode,
      prompt,
    };
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
}


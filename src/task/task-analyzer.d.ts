/**
 * 任务分析器
 * 解析用户输入，识别任务类型、复杂度和目标文件
 */
import { TaskCategory } from '../types';
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
/**
 * 任务分析器类
 */
export declare class TaskAnalyzer {
    /**
     * 分析用户输入
     */
    analyze(prompt: string): TaskAnalysis;
    /**
     * 检测任务类型
     */
    private detectCategory;
    /**
     * 提取目标文件
     */
    private extractTargetFiles;
    /**
     * 提取关键词
     */
    private extractKeywords;
    /**
     * 评估复杂度 (1-5)
     */
    private assessComplexity;
    /**
     * 判断是否可拆分
     */
    private isSplittable;
    /**
     * 建议执行模式
     */
    private suggestMode;
}
//# sourceMappingURL=task-analyzer.d.ts.map
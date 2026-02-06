/**
 * Task Pre-Analyzer - 任务预分析器
 *
 * 职责：
 * - 在执行前分析任务特性
 * - 决定执行策略（需要哪些阶段）
 * - 评估任务复杂度
 * - 推荐执行模式
 *
 * 这是实现"智能编排"的核心组件，让编排者在执行前"思考"
 */

import { Mission, Assignment } from '../../mission';
import { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import { logger, LogCategory } from '../../../logging';
import { TokenUsage } from '../../../types/agent-types';

/**
 * 任务复杂度级别
 */
export enum TaskComplexity {
  /** 简单任务：单文件修改、小改动 */
  SIMPLE = 'simple',
  /** 中等任务：多文件修改、需要协调 */
  MODERATE = 'moderate',
  /** 复杂任务：架构变更、多 Worker 协作 */
  COMPLEX = 'complex',
}

/**
 * 执行策略
 */
export interface ExecutionStrategy {
  /** 任务复杂度 */
  complexity: TaskComplexity;

  /** 是否需要规划阶段 */
  needsPlanning: boolean;

  /** 是否需要评审阶段 */
  needsReview: boolean;

  /** 是否需要验证阶段 */
  needsVerification: boolean;

  /** 是否并行执行 */
  parallel: boolean;

  /** 策略说明 */
  reasoning: string;

  /** 编排者的分析摘要（用于 UI 展示） */
  analysisSummary: string;

  /** Token 使用统计 */
  tokenUsage?: TokenUsage;
}

/**
 * 预分析选项
 */
export interface PreAnalysisOptions {
  /** 项目上下文 */
  projectContext?: string;
}

/**
 * 任务预分析器
 */
export class TaskPreAnalyzer {
  constructor(private adapterFactory: IAdapterFactory) {}

  /**
   * 分析任务并决定执行策略
   */
  async analyze(
    mission: Mission,
    options: PreAnalysisOptions = {}
  ): Promise<ExecutionStrategy> {
    logger.info('任务预分析.开始', { missionId: mission.id, goal: mission.goal }, LogCategory.ORCHESTRATOR);

    const llmStrategy = await this.analyzeWithLLM(mission, options);
    logger.info('任务预分析.LLM分析', { strategy: llmStrategy }, LogCategory.ORCHESTRATOR);
    return llmStrategy;
  }

  /**
   * 使用 LLM 进行深度分析
   */
  private async analyzeWithLLM(
    mission: Mission,
    options: PreAnalysisOptions
  ): Promise<ExecutionStrategy> {
    const prompt = this.buildAnalysisPrompt(mission, options);

    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      prompt,
      undefined,
      {
        adapterRole: 'orchestrator',
      }
    );

    try {
      const parsed = this.parseAnalysisResponse(response.content);
      return {
        ...parsed,
        tokenUsage: response.tokenUsage,
      };
    } catch (error) {
      logger.error('任务预分析.解析失败', { error, response: response.content }, LogCategory.ORCHESTRATOR);
      throw new Error('任务预分析解析失败');
    }
  }

  /**
   * 构建分析提示词
   */
  private buildAnalysisPrompt(mission: Mission, options: PreAnalysisOptions): string {
    const assignmentSummaries = mission.assignments.map((a, i) =>
      `${i + 1}. [${a.workerId}] ${a.responsibility}`
    ).join('\n');

    return `你是一个智能任务编排者。分析以下任务并决定最佳执行策略。

## 任务目标
${mission.goal}

## 已分配的子任务
${assignmentSummaries}

## 契约依赖
${mission.contracts?.length ? mission.contracts.map(c => `- ${c.description || c.id}`).join('\n') : '无'}

${options.projectContext ? `## 项目上下文\n${options.projectContext}` : ''}

## 输出格式

**重要：为了让用户理解你的分析过程，请先用自然语言解释你的思考，然后输出 JSON。**

格式如下：

### 任务分析
[用 2-3 句话向用户解释你对这个任务的理解、复杂度评估和执行策略选择的理由]

### 执行策略
\`\`\`json
{
  "complexity": "simple|moderate|complex",
  "needsPlanning": boolean,
  "needsReview": boolean,
  "needsVerification": boolean,
  "parallel": boolean,
  "reasoning": "你的分析理由",
  "analysisSummary": "简短的分析摘要（用于展示给用户，带 emoji）"
}
\`\`\`

决策指南：
- 简单任务（1个子任务，无依赖）：跳过规划和评审
- 中等任务（2-3个子任务，无复杂依赖）：需要规划，可跳过评审
- 复杂任务（多子任务，有依赖）：需要完整流程
- 如果子任务间无依赖，建议并行执行`;
  }

  /**
   * 解析 LLM 响应
   */
  private parseAnalysisResponse(content: string): Omit<ExecutionStrategy, 'tokenUsage'> {
    // 提取 JSON 块
    const jsonMatch = content.match(/```json\s*([\s\S]*?)\s*```/);
    const jsonStr = jsonMatch ? jsonMatch[1] : content;

    const parsed = JSON.parse(jsonStr);

    // 验证并规范化
    const complexity = this.normalizeComplexity(parsed.complexity);

    return {
      complexity,
      needsPlanning: Boolean(parsed.needsPlanning),
      needsReview: Boolean(parsed.needsReview),
      needsVerification: Boolean(parsed.needsVerification),
      parallel: Boolean(parsed.parallel),
      reasoning: String(parsed.reasoning || ''),
      analysisSummary: String(parsed.analysisSummary || '任务分析完成'),
    };
  }

  /**
   * 规范化复杂度值
   */
  private normalizeComplexity(value: string): TaskComplexity {
    const normalized = String(value).toLowerCase();
    if (normalized === 'simple') return TaskComplexity.SIMPLE;
    if (normalized === 'moderate') return TaskComplexity.MODERATE;
    return TaskComplexity.COMPLEX;
  }
}

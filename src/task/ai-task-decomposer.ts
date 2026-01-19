/**
 * AI 任务分解器
 * 调用 AI CLI 分析复杂任务并自动分解为子任务
 */

import { logger, LogCategory } from '../logging';
import { CLIType, TaskCategory } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { TaskAnalysis } from './task-analyzer';
import { SubTaskDef, SplitResult } from './task-splitter';
import { CLISelector } from './cli-selector';

/** AI 分解配置 */
export interface AIDecomposeConfig {
  /** 用于分解任务的 CLI（默认 claude） */
  decomposeCli: CLIType;
  /** 复杂度阈值，超过此值才使用 AI 分解 */
  complexityThreshold: number;
  /** 超时时间（毫秒） */
  timeout: number;
}

const DEFAULT_CONFIG: AIDecomposeConfig = {
  decomposeCli: 'claude',
  complexityThreshold: 3,
  timeout: 60000,
};

/** AI 分解的子任务结构 */
interface AISubTask {
  description: string;
  category: string;
  targetFiles: string[];
  dependencies: string[];
  priority: number;
}

/**
 * AI 任务分解器
 */
export class AITaskDecomposer {
  private cliFactory: CLIAdapterFactory;
  private cliSelector: CLISelector;
  private config: AIDecomposeConfig;

  constructor(
    cliFactory: CLIAdapterFactory,
    cliSelector: CLISelector,
    config?: Partial<AIDecomposeConfig>
  ) {
    this.cliFactory = cliFactory;
    this.cliSelector = cliSelector;
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 判断是否需要 AI 分解
   */
  shouldUseAI(analysis: TaskAnalysis): boolean {
    return analysis.splittable && analysis.complexity >= this.config.complexityThreshold;
  }

  /**
   * 使用 AI 分解任务
   */
  async decompose(analysis: TaskAnalysis): Promise<SplitResult> {
    const prompt = this.buildDecomposePrompt(analysis);
    
    try {
      const response = await this.cliFactory.sendMessage(
        this.config.decomposeCli,
        prompt
      );

      if (response.error) {
        logger.error('任务.分解器.AI_失败', { error: response.error }, LogCategory.TASK);
        return this.fallbackSplit(analysis);
      }

      const subTasks = this.parseAIResponse(response.content, analysis);
      if (subTasks.length === 0) {
        return this.fallbackSplit(analysis);
      }

      return {
        subTasks,
        executionMode: this.determineExecutionMode(subTasks),
        estimatedTime: subTasks.length * 30,
        hasDependencies: subTasks.some(t => t.dependencies.length > 0),
      };
    } catch (error) {
      logger.error('任务.分解器.异常', error, LogCategory.TASK);
      return this.fallbackSplit(analysis);
    }
  }

  /**
   * 构建分解提示词
   */
  private buildDecomposePrompt(analysis: TaskAnalysis): string {
    return `请分析以下编程任务，将其分解为可独立执行的子任务。

任务描述：${analysis.prompt}
任务类型：${analysis.category}
目标文件：${analysis.targetFiles.join(', ') || '未指定'}
复杂度：${analysis.complexity}/5

请以 JSON 格式返回子任务列表，格式如下：
\`\`\`json
{
  "subTasks": [
    {
      "description": "子任务描述",
      "category": "任务类型(architecture/implement/bugfix/frontend/test/document)",
      "targetFiles": ["file1.ts", "file2.ts"],
      "dependencies": [],
      "priority": 1
    }
  ]
}
\`\`\`

要求：
1. 每个子任务应该是独立可执行的
2. 如果有依赖关系，在 dependencies 中填写依赖的子任务索引（从0开始）
3. priority 表示优先级，1最高
4. 尽量将任务拆分为可并行执行的部分`;
  }

  /**
   * 解析 AI 响应
   */
  private parseAIResponse(content: string, analysis: TaskAnalysis): SubTaskDef[] {
    try {
      // 提取 JSON 块
      const jsonMatch = content.match(/```json\s*([\s\S]*?)\s*```/);
      const jsonStr = jsonMatch ? jsonMatch[1] : content;
      
      const parsed = JSON.parse(jsonStr);
      const aiSubTasks: AISubTask[] = parsed.subTasks || [];
      const baseId = `ai-${Date.now()}`;

      return aiSubTasks.map((task, index) => {
        const category = this.normalizeCategory(task.category);
        const selection = this.cliSelector.selectByCategory(category);
        const id = `${baseId}-${index}`;
        
        return {
          id,
          description: task.description,
          category,
          assignedWorker: selection.cli,
          targetFiles: task.targetFiles || [],
          dependencies: (task.dependencies || []).map((d: string | number) => `${baseId}-${d}`),
          priority: task.priority || index + 1,
          cliSelection: selection,
        };
      });
    } catch (error) {
      logger.error('任务.分解器.解析_失败', error, LogCategory.TASK);
      return [];
    }
  }

  /**
   * 标准化任务类型
   */
  private normalizeCategory(category: string): TaskCategory {
    const categoryMap: Record<string, TaskCategory> = {
      'architecture': 'architecture',
      'implement': 'implement',
      'implementation': 'implement',
      'bugfix': 'bugfix',
      'bug': 'bugfix',
      'fix': 'bugfix',
      'frontend': 'frontend',
      'ui': 'frontend',
      'test': 'test',
      'testing': 'test',
      'document': 'document',
      'docs': 'document',
      'refactor': 'refactor',
      'debug': 'debug',
      'review': 'review',
    };
    return categoryMap[category.toLowerCase()] || 'general';
  }

  /**
   * 确定执行模式
   */
  private determineExecutionMode(subTasks: SubTaskDef[]): 'sequential' | 'parallel' | 'mixed' {
    const hasDeps = subTasks.some(t => t.dependencies.length > 0);
    if (!hasDeps) return 'parallel';

    // 检查是否所有任务都有依赖（纯串行）
    const allHaveDeps = subTasks.slice(1).every(t => t.dependencies.length > 0);
    if (allHaveDeps) return 'sequential';

    return 'mixed';
  }

  /**
   * 降级到规则分解
   */
  private fallbackSplit(analysis: TaskAnalysis): SplitResult {
    const selection = this.cliSelector.select(analysis);
    return {
      subTasks: [{
        id: `fallback-${Date.now()}`,
        description: analysis.prompt,
        category: analysis.category,
        assignedWorker: selection.cli,
        targetFiles: analysis.targetFiles,
        dependencies: [],
        priority: 1,
        cliSelection: selection,
      }],
      executionMode: 'sequential',
      estimatedTime: analysis.complexity * 30,
      hasDependencies: false,
    };
  }

  /**
   * 更新配置
   */
  updateConfig(config: Partial<AIDecomposeConfig>): void {
    this.config = { ...this.config, ...config };
  }
}

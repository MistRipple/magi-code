"use strict";
/**
 * AI 任务分解器
 * 调用 AI CLI 分析复杂任务并自动分解为子任务
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.AITaskDecomposer = void 0;
const DEFAULT_CONFIG = {
    decomposeCli: 'claude',
    complexityThreshold: 3,
    timeout: 60000,
};
/**
 * AI 任务分解器
 */
class AITaskDecomposer {
    cliFactory;
    cliSelector;
    config;
    constructor(cliFactory, cliSelector, config) {
        this.cliFactory = cliFactory;
        this.cliSelector = cliSelector;
        this.config = { ...DEFAULT_CONFIG, ...config };
    }
    /**
     * 判断是否需要 AI 分解
     */
    shouldUseAI(analysis) {
        return analysis.splittable && analysis.complexity >= this.config.complexityThreshold;
    }
    /**
     * 使用 AI 分解任务
     */
    async decompose(analysis) {
        const prompt = this.buildDecomposePrompt(analysis);
        try {
            const response = await this.cliFactory.sendMessage(this.config.decomposeCli, prompt);
            if (response.error) {
                console.error('[AITaskDecomposer] AI 分解失败:', response.error);
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
        }
        catch (error) {
            console.error('[AITaskDecomposer] AI 分解异常:', error);
            return this.fallbackSplit(analysis);
        }
    }
    /**
     * 构建分解提示词
     */
    buildDecomposePrompt(analysis) {
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
    parseAIResponse(content, analysis) {
        try {
            // 提取 JSON 块
            const jsonMatch = content.match(/```json\s*([\s\S]*?)\s*```/);
            const jsonStr = jsonMatch ? jsonMatch[1] : content;
            const parsed = JSON.parse(jsonStr);
            const aiSubTasks = parsed.subTasks || [];
            return aiSubTasks.map((task, index) => {
                const category = this.normalizeCategory(task.category);
                const selection = this.cliSelector.selectByCategory(category);
                return {
                    id: `ai-${Date.now()}-${index}`,
                    description: task.description,
                    category,
                    assignedCli: selection.cli,
                    targetFiles: task.targetFiles || [],
                    dependencies: (task.dependencies || []).map((d) => `ai-${Date.now()}-${d}`),
                    priority: task.priority || index + 1,
                    cliSelection: selection,
                };
            });
        }
        catch (error) {
            console.error('[AITaskDecomposer] 解析 AI 响应失败:', error);
            return [];
        }
    }
    /**
     * 标准化任务类型
     */
    normalizeCategory(category) {
        const categoryMap = {
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
    determineExecutionMode(subTasks) {
        const hasDeps = subTasks.some(t => t.dependencies.length > 0);
        if (!hasDeps)
            return 'parallel';
        // 检查是否所有任务都有依赖（纯串行）
        const allHaveDeps = subTasks.slice(1).every(t => t.dependencies.length > 0);
        if (allHaveDeps)
            return 'sequential';
        return 'mixed';
    }
    /**
     * 降级到规则分解
     */
    fallbackSplit(analysis) {
        const selection = this.cliSelector.select(analysis);
        return {
            subTasks: [{
                    id: `fallback-${Date.now()}`,
                    description: analysis.prompt,
                    category: analysis.category,
                    assignedCli: selection.cli,
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
    updateConfig(config) {
        this.config = { ...this.config, ...config };
    }
}
exports.AITaskDecomposer = AITaskDecomposer;
//# sourceMappingURL=ai-task-decomposer.js.map
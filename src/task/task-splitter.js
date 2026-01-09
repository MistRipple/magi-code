"use strict";
/**
 * 任务拆分器
 * 将复杂任务拆分为子任务，标注依赖关系
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.TaskSplitter = void 0;
function generateId() {
    return `st-${Date.now()}-${Math.random().toString(36).substring(2, 7)}`;
}
class TaskSplitter {
    cliSelector;
    constructor(cliSelector) {
        this.cliSelector = cliSelector;
    }
    split(analysis) {
        if (!analysis.splittable)
            return this.createSingleTask(analysis);
        switch (analysis.category) {
            case 'architecture': return this.splitArchitectureTask(analysis);
            case 'implement': return this.splitImplementTask(analysis);
            default: return this.splitByFiles(analysis);
        }
    }
    createSingleTask(analysis) {
        const selection = this.cliSelector.select(analysis);
        return {
            subTasks: [{
                    id: generateId(), description: analysis.prompt, category: analysis.category,
                    assignedCli: selection.cli, targetFiles: analysis.targetFiles,
                    dependencies: [], priority: 1, cliSelection: selection,
                }],
            executionMode: 'sequential',
            estimatedTime: this.estimateTime(analysis.complexity),
            hasDependencies: false,
        };
    }
    splitByFiles(analysis) {
        const files = analysis.targetFiles;
        if (files.length <= 1)
            return this.createSingleTask(analysis);
        const subTasks = files.map((file, index) => {
            const selection = this.cliSelector.select(analysis);
            return {
                id: generateId(), description: `处理文件: ${file}`, category: analysis.category,
                assignedCli: selection.cli, targetFiles: [file],
                dependencies: [], priority: index + 1, cliSelection: selection,
            };
        });
        return { subTasks, executionMode: 'parallel', estimatedTime: this.estimateTime(analysis.complexity), hasDependencies: false };
    }
    splitArchitectureTask(analysis) {
        const subTasks = [];
        const designSelection = this.cliSelector.selectByCategory('architecture');
        const designTask = {
            id: generateId(), description: `分析需求并设计架构: ${analysis.prompt}`,
            category: 'architecture', assignedCli: designSelection.cli, targetFiles: [],
            dependencies: [], priority: 1, cliSelection: designSelection,
        };
        subTasks.push(designTask);
        const implSelection = this.cliSelector.selectByCategory('implement');
        subTasks.push({
            id: generateId(), description: `实现架构设计`, category: 'implement',
            assignedCli: implSelection.cli, targetFiles: analysis.targetFiles,
            dependencies: [designTask.id], priority: 2, cliSelection: implSelection,
        });
        return { subTasks, executionMode: 'sequential', estimatedTime: this.estimateTime(analysis.complexity) * 1.5, hasDependencies: true };
    }
    splitImplementTask(analysis) {
        const hasFrontend = analysis.keywords.some(k => ['前端', 'frontend', 'ui', 'css', 'component'].includes(k));
        const hasBackend = analysis.keywords.some(k => ['后端', 'backend', 'api', '服务', 'server'].includes(k));
        if (hasFrontend && hasBackend)
            return this.splitFullStackTask(analysis);
        return this.splitByFiles(analysis);
    }
    splitFullStackTask(analysis) {
        const subTasks = [];
        const backendSelection = this.cliSelector.selectByCategory('implement');
        subTasks.push({
            id: generateId(), description: `实现后端 API: ${analysis.prompt}`, category: 'implement',
            assignedCli: backendSelection.cli,
            targetFiles: analysis.targetFiles.filter(f => !f.includes('component') && !f.includes('.css') && !f.includes('.tsx')),
            dependencies: [], priority: 1, cliSelection: backendSelection,
        });
        const frontendSelection = this.cliSelector.selectByCategory('frontend');
        subTasks.push({
            id: generateId(), description: `实现前端界面: ${analysis.prompt}`, category: 'frontend',
            assignedCli: frontendSelection.cli,
            targetFiles: analysis.targetFiles.filter(f => f.includes('component') || f.includes('.css') || f.includes('.tsx')),
            dependencies: [], priority: 1, cliSelection: frontendSelection,
        });
        return { subTasks, executionMode: 'parallel', estimatedTime: this.estimateTime(analysis.complexity), hasDependencies: false };
    }
    estimateTime(complexity) { return 30 * complexity; }
}
exports.TaskSplitter = TaskSplitter;
//# sourceMappingURL=task-splitter.js.map
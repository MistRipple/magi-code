/**
 * 任务拆分器
 * 将复杂任务拆分为子任务，标注依赖关系
 */

import { WorkerSlot, TaskCategory } from '../types';
import { TaskAnalysis } from './task-analyzer';
import { WorkerSelector, WorkerSelection } from './worker-selector';

/** 子任务定义 */
export interface SubTaskDef {
  id: string;
  description: string;
  category: TaskCategory;
  assignedWorker: WorkerSlot;
  targetFiles: string[];
  dependencies: string[];
  priority: number;
  workerSelection: WorkerSelection;
}

/** 拆分结果 */
export interface SplitResult {
  subTasks: SubTaskDef[];
  executionMode: 'sequential' | 'parallel' | 'mixed';
  estimatedTime: number;
  hasDependencies: boolean;
}

function generateId(): string {
  return `st-${Date.now()}-${Math.random().toString(36).substring(2, 7)}`;
}

export class TaskSplitter {
  private workerSelector: WorkerSelector;

  constructor(workerSelector: WorkerSelector) {
    this.workerSelector = workerSelector;
  }

  split(analysis: TaskAnalysis): SplitResult {
    if (!analysis.splittable) return this.createSingleTask(analysis);
    switch (analysis.category) {
      case 'architecture': return this.splitArchitectureTask(analysis);
      case 'implement':
      case 'backend':
      case 'frontend':
        return this.splitImplementTask(analysis);
      default: return this.splitByFiles(analysis);
    }
  }

  private createSingleTask(analysis: TaskAnalysis): SplitResult {
    // Use recommendedWorker from profile system as preference
    const selection = this.workerSelector.select(analysis, analysis.recommendedWorker);
    return {
      subTasks: [{
        id: generateId(), description: analysis.prompt, category: analysis.category,
        assignedWorker: selection.worker, targetFiles: analysis.targetFiles,
        dependencies: [], priority: 1, workerSelection: selection,
      }],
      executionMode: 'sequential',
      estimatedTime: this.estimateTime(analysis.complexity),
      hasDependencies: false,
    };
  }

  private splitByFiles(analysis: TaskAnalysis): SplitResult {
    const files = analysis.targetFiles;
    if (files.length <= 1) return this.createSingleTask(analysis);
    const subTasks: SubTaskDef[] = files.map((file, index) => {
      // Use recommendedWorker from profile system as preference
      const selection = this.workerSelector.select(analysis, analysis.recommendedWorker);
      return {
        id: generateId(), description: `处理文件: ${file}`, category: analysis.category,
        assignedWorker: selection.worker, targetFiles: [file],
        dependencies: [], priority: index + 1, workerSelection: selection,
      };
    });
    return { subTasks, executionMode: 'parallel', estimatedTime: this.estimateTime(analysis.complexity), hasDependencies: false };
  }

  private splitArchitectureTask(analysis: TaskAnalysis): SplitResult {
    const subTasks: SubTaskDef[] = [];
    const designSelection = this.workerSelector.selectByCategory('architecture');
    const designTask: SubTaskDef = {
      id: generateId(), description: `分析需求并设计架构: ${analysis.prompt}`,
      category: 'architecture', assignedWorker: designSelection.worker, targetFiles: [],
      dependencies: [], priority: 1, workerSelection: designSelection,
    };
    subTasks.push(designTask);
    const implSelection = this.workerSelector.selectByCategory('implement');
    subTasks.push({
      id: generateId(), description: `实现架构设计`, category: 'implement',
      assignedWorker: implSelection.worker, targetFiles: analysis.targetFiles,
      dependencies: [designTask.id], priority: 2, workerSelection: implSelection,
    });
    return { subTasks, executionMode: 'sequential', estimatedTime: this.estimateTime(analysis.complexity) * 1.5, hasDependencies: true };
  }

  private splitImplementTask(analysis: TaskAnalysis): SplitResult {
    const hasFrontend = analysis.keywords.some(k => ['前端', 'frontend', 'ui', 'css', 'component'].includes(k));
    const hasBackend = analysis.keywords.some(k => ['后端', 'backend', 'api', '服务', 'server'].includes(k));
    if (hasFrontend && hasBackend) return this.splitFullStackTask(analysis);
    return this.splitByFiles(analysis);
  }

  private splitFullStackTask(analysis: TaskAnalysis): SplitResult {
    const subTasks: SubTaskDef[] = [];
    const backendSelection = this.workerSelector.selectByCategory('backend');
    subTasks.push({
      id: generateId(), description: `实现后端 API: ${analysis.prompt}`, category: 'backend',
      assignedWorker: backendSelection.worker,
      targetFiles: analysis.targetFiles.filter(f => !f.includes('component') && !f.includes('.css') && !f.includes('.tsx')),
      dependencies: [], priority: 1, workerSelection: backendSelection,
    });
    const frontendSelection = this.workerSelector.selectByCategory('frontend');
    subTasks.push({
      id: generateId(), description: `实现前端界面: ${analysis.prompt}`, category: 'frontend',
      assignedWorker: frontendSelection.worker,
      targetFiles: analysis.targetFiles.filter(f => f.includes('component') || f.includes('.css') || f.includes('.tsx')),
      dependencies: [], priority: 1, workerSelection: frontendSelection,
    });
    return { subTasks, executionMode: 'parallel', estimatedTime: this.estimateTime(analysis.complexity), hasDependencies: false };
  }

  private estimateTime(complexity: number): number { return 30 * complexity; }
}

/**
 * 任务拆分器
 * 将复杂任务拆分为子任务，标注依赖关系
 */
import { CLIType, TaskCategory } from '../types';
import { TaskAnalysis } from './task-analyzer';
import { CLISelector, CLISelection } from './cli-selector';
/** 子任务定义 */
export interface SubTaskDef {
    id: string;
    description: string;
    category: TaskCategory;
    assignedCli: CLIType;
    targetFiles: string[];
    dependencies: string[];
    priority: number;
    cliSelection: CLISelection;
}
/** 拆分结果 */
export interface SplitResult {
    subTasks: SubTaskDef[];
    executionMode: 'sequential' | 'parallel' | 'mixed';
    estimatedTime: number;
    hasDependencies: boolean;
}
export declare class TaskSplitter {
    private cliSelector;
    constructor(cliSelector: CLISelector);
    split(analysis: TaskAnalysis): SplitResult;
    private createSingleTask;
    private splitByFiles;
    private splitArchitectureTask;
    private splitImplementTask;
    private splitFullStackTask;
    private estimateTime;
}
//# sourceMappingURL=task-splitter.d.ts.map
/**
 * CLI 选择器
 * 根据任务类型、用户配置和 CLI 可用性选择最佳 CLI
 */
import { CLIType, TaskCategory } from '../types';
import { TaskAnalysis } from './task-analyzer';
/** CLI 能力配置 */
export interface CLISkillsConfig {
    architecture: CLIType;
    implement: CLIType;
    refactor: CLIType;
    bugfix: CLIType;
    debug: CLIType;
    frontend: CLIType;
    test: CLIType;
    document: CLIType;
    review: CLIType;
    general: CLIType;
}
/** CLI 选择结果 */
export interface CLISelection {
    /** 选中的 CLI */
    cli: CLIType;
    /** 是否为降级选择 */
    degraded: boolean;
    /** 原始首选 CLI */
    preferred: CLIType;
    /** 选择原因 */
    reason: string;
}
/**
 * CLI 选择器类
 */
export declare class CLISelector {
    private skills;
    private availableCLIs;
    constructor(skills?: Partial<CLISkillsConfig>);
    /**
     * 更新可用 CLI 列表
     */
    setAvailableCLIs(clis: CLIType[]): void;
    /**
     * 更新技能配置
     */
    updateSkills(skills: Partial<CLISkillsConfig>): void;
    /**
     * 根据任务分析选择最佳 CLI
     */
    select(analysis: TaskAnalysis): CLISelection;
    /**
     * 根据任务类型直接选择 CLI
     */
    selectByCategory(category: TaskCategory): CLISelection;
    /**
     * 获取当前技能配置
     */
    getSkills(): CLISkillsConfig;
    /**
     * 获取可用 CLI 列表
     */
    getAvailableCLIs(): CLIType[];
}
//# sourceMappingURL=cli-selector.d.ts.map
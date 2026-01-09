/**
 * CLI 检测器模块
 * 负责检测各 CLI 工具的可用性并制定降级策略
 * 版本: 0.3.0 - 添加健康检查和事件发射
 */
import { CLIType, CLIStatus, DegradationStrategy, TaskCategory } from './types';
export declare class CLIDetector {
    private config;
    private statusCache;
    private cacheExpiry;
    private lastCheck;
    private healthCheckInterval;
    private healthCheckPeriod;
    constructor();
    /**
     * 启动健康检查定时器
     */
    startHealthCheck(): void;
    /**
     * 停止健康检查定时器
     */
    stopHealthCheck(): void;
    /**
     * 获取 CLI 路径配置
     */
    private getCLIPath;
    /**
     * 检测单个 CLI 的可用性 (支持更细粒度状态)
     */
    checkCLI(type: CLIType): Promise<CLIStatus>;
    /**
     * 检测所有 CLI
     */
    checkAllCLIs(forceRefresh?: boolean): Promise<CLIStatus[]>;
    /**
     * 制定降级策略 (包含能力分配)
     */
    getDegradationStrategy(): Promise<DegradationStrategy>;
    /**
     * 构建降级策略 (核心逻辑)
     */
    private buildStrategy;
    /**
     * 获取任务的最佳处理 CLI
     */
    getHandlerForTask(taskType: TaskCategory, strategy: DegradationStrategy): CLIType | null;
    /**
     * 解析版本号
     */
    private parseVersion;
    /**
     * 比较版本号
     * @returns -1 if v1 < v2, 0 if equal, 1 if v1 > v2
     */
    private compareVersions;
    /**
     * 检查版本是否满足最低要求
     */
    checkVersionRequirement(type: CLIType, version: string): {
        meets: boolean;
        minVersion: string;
    };
    /**
     * 获取所有 CLI 的详细状态摘要
     */
    getStatusSummary(): Promise<{
        available: number;
        total: number;
        statuses: CLIStatus[];
        recommendation: string;
    }>;
    /**
     * 解析错误类型 (更细粒度)
     */
    private parseError;
    /**
     * 获取安装指引
     */
    getInstallGuide(type: CLIType): string;
    /**
     * 刷新配置
     */
    refreshConfig(): void;
}
export declare const cliDetector: CLIDetector;
//# sourceMappingURL=cli-detector.d.ts.map
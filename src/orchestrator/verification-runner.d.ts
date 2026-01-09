/**
 * 验证执行器
 * 负责执行 Phase 4 的验证检查：编译、Lint、测试、IDE 诊断
 */
/** 验证配置 */
export interface VerificationConfig {
    /** 编译检查（默认 true） */
    compileCheck: boolean;
    /** 编译命令（默认 npm run compile） */
    compileCommand: string;
    /** IDE 诊断检查（默认 true） */
    ideCheck: boolean;
    /** Lint 检查（默认 false） */
    lintCheck: boolean;
    /** Lint 命令（默认 npm run lint） */
    lintCommand: string;
    /** 测试检查（默认 false） */
    testCheck: boolean;
    /** 测试命令（默认 npm test） */
    testCommand: string;
    /** 验证超时时间（默认 60000ms） */
    timeout: number;
}
/** 验证结果 */
export interface VerificationResult {
    success: boolean;
    compileResult?: CommandResult;
    lintResult?: CommandResult;
    testResult?: CommandResult;
    ideResult?: IDEDiagnosticResult;
    summary: string;
}
/** 命令执行结果 */
export interface CommandResult {
    success: boolean;
    output: string;
    error?: string;
    duration: number;
}
/** IDE 诊断结果 */
export interface IDEDiagnosticResult {
    success: boolean;
    errors: number;
    warnings: number;
    details: Array<{
        file: string;
        line: number;
        message: string;
        severity: 'error' | 'warning';
    }>;
}
/**
 * 验证执行器
 */
export declare class VerificationRunner {
    private config;
    private workspaceRoot;
    constructor(workspaceRoot: string, config?: Partial<VerificationConfig>);
    /** 更新配置 */
    updateConfig(config: Partial<VerificationConfig>): void;
    /**
     * 执行完整验证流程
     */
    runVerification(taskId: string, modifiedFiles?: string[]): Promise<VerificationResult>;
    /**
     * 执行命令并返回结果
     */
    private runCommand;
    /**
     * 执行 IDE 诊断检查
     */
    private runIDEDiagnostics;
    /**
     * 快速编译检查
     */
    quickCompileCheck(): Promise<boolean>;
    /**
     * 获取错误详情（用于恢复阶段）
     */
    getErrorDetails(result: VerificationResult): string;
}
//# sourceMappingURL=verification-runner.d.ts.map
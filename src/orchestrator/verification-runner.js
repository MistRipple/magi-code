"use strict";
/**
 * 验证执行器
 * 负责执行 Phase 4 的验证检查：编译、Lint、测试、IDE 诊断
 */
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.VerificationRunner = void 0;
const child_process_1 = require("child_process");
const vscode = __importStar(require("vscode"));
const events_1 = require("../events");
const DEFAULT_CONFIG = {
    compileCheck: true,
    compileCommand: 'npm run compile',
    ideCheck: true,
    lintCheck: false,
    lintCommand: 'npm run lint',
    testCheck: false,
    testCommand: 'npm test',
    timeout: 60000,
};
/**
 * 验证执行器
 */
class VerificationRunner {
    config;
    workspaceRoot;
    constructor(workspaceRoot, config) {
        this.workspaceRoot = workspaceRoot;
        this.config = { ...DEFAULT_CONFIG, ...config };
    }
    /** 更新配置 */
    updateConfig(config) {
        this.config = { ...this.config, ...config };
    }
    /**
     * 执行完整验证流程
     */
    async runVerification(taskId, modifiedFiles) {
        console.log('[VerificationRunner] 开始验证检查...');
        events_1.globalEventBus.emitEvent('verification:started', { taskId });
        const result = {
            success: true,
            summary: '',
        };
        const summaryParts = [];
        // 1. 编译检查
        if (this.config.compileCheck) {
            console.log('[VerificationRunner] 执行编译检查...');
            result.compileResult = await this.runCommand(this.config.compileCommand, '编译');
            if (!result.compileResult.success) {
                result.success = false;
                summaryParts.push(`❌ 编译失败: ${result.compileResult.error || '未知错误'}`);
            }
            else {
                summaryParts.push('✅ 编译通过');
            }
        }
        // 2. IDE 诊断检查
        if (this.config.ideCheck) {
            console.log('[VerificationRunner] 执行 IDE 诊断检查...');
            result.ideResult = await this.runIDEDiagnostics(modifiedFiles);
            if (!result.ideResult.success) {
                result.success = false;
                summaryParts.push(`❌ IDE 诊断: ${result.ideResult.errors} 个错误`);
            }
            else {
                const warningText = result.ideResult.warnings > 0
                    ? ` (${result.ideResult.warnings} 个警告)`
                    : '';
                summaryParts.push(`✅ IDE 诊断通过${warningText}`);
            }
        }
        // 3. Lint 检查
        if (this.config.lintCheck) {
            console.log('[VerificationRunner] 执行 Lint 检查...');
            result.lintResult = await this.runCommand(this.config.lintCommand, 'Lint');
            if (!result.lintResult.success) {
                result.success = false;
                summaryParts.push(`❌ Lint 失败: ${result.lintResult.error || '未知错误'}`);
            }
            else {
                summaryParts.push('✅ Lint 通过');
            }
        }
        // 4. 测试检查
        if (this.config.testCheck) {
            console.log('[VerificationRunner] 执行测试检查...');
            result.testResult = await this.runCommand(this.config.testCommand, '测试');
            if (!result.testResult.success) {
                result.success = false;
                summaryParts.push(`❌ 测试失败: ${result.testResult.error || '未知错误'}`);
            }
            else {
                summaryParts.push('✅ 测试通过');
            }
        }
        result.summary = summaryParts.join(' | ');
        events_1.globalEventBus.emitEvent('verification:completed', {
            taskId,
            data: { success: result.success, summary: result.summary }
        });
        console.log(`[VerificationRunner] 验证完成: ${result.success ? '通过' : '失败'}`);
        return result;
    }
    /**
     * 执行命令并返回结果
     */
    async runCommand(command, name) {
        const startTime = Date.now();
        return new Promise((resolve) => {
            const [cmd, ...args] = command.split(' ');
            const process = (0, child_process_1.spawn)(cmd, args, {
                cwd: this.workspaceRoot,
                shell: true,
                timeout: this.config.timeout,
            });
            let stdout = '';
            let stderr = '';
            process.stdout?.on('data', (data) => {
                stdout += data.toString();
            });
            process.stderr?.on('data', (data) => {
                stderr += data.toString();
            });
            process.on('close', (code) => {
                const duration = Date.now() - startTime;
                resolve({
                    success: code === 0,
                    output: stdout,
                    error: code !== 0 ? stderr || `${name}失败，退出码: ${code}` : undefined,
                    duration,
                });
            });
            process.on('error', (err) => {
                const duration = Date.now() - startTime;
                resolve({
                    success: false,
                    output: '',
                    error: `${name}执行错误: ${err.message}`,
                    duration,
                });
            });
        });
    }
    /**
     * 执行 IDE 诊断检查
     */
    async runIDEDiagnostics(modifiedFiles) {
        const result = {
            success: true,
            errors: 0,
            warnings: 0,
            details: [],
        };
        try {
            // 获取所有诊断信息
            const allDiagnostics = vscode.languages.getDiagnostics();
            for (const [uri, diagnostics] of allDiagnostics) {
                // 如果指定了修改的文件，只检查这些文件
                if (modifiedFiles && modifiedFiles.length > 0) {
                    const filePath = uri.fsPath;
                    const isModified = modifiedFiles.some(f => filePath.endsWith(f) || filePath.includes(f));
                    if (!isModified)
                        continue;
                }
                for (const diagnostic of diagnostics) {
                    if (diagnostic.severity === vscode.DiagnosticSeverity.Error) {
                        result.errors++;
                        result.details.push({
                            file: uri.fsPath,
                            line: diagnostic.range.start.line + 1,
                            message: diagnostic.message,
                            severity: 'error',
                        });
                    }
                    else if (diagnostic.severity === vscode.DiagnosticSeverity.Warning) {
                        result.warnings++;
                        result.details.push({
                            file: uri.fsPath,
                            line: diagnostic.range.start.line + 1,
                            message: diagnostic.message,
                            severity: 'warning',
                        });
                    }
                }
            }
            result.success = result.errors === 0;
        }
        catch (error) {
            console.error('[VerificationRunner] IDE 诊断检查失败:', error);
            result.success = false;
        }
        return result;
    }
    /**
     * 快速编译检查
     */
    async quickCompileCheck() {
        if (!this.config.compileCheck)
            return true;
        const result = await this.runCommand(this.config.compileCommand, '编译');
        return result.success;
    }
    /**
     * 获取错误详情（用于恢复阶段）
     */
    getErrorDetails(result) {
        const details = [];
        if (result.compileResult && !result.compileResult.success) {
            details.push(`编译错误:\n${result.compileResult.error || result.compileResult.output}`);
        }
        if (result.ideResult && !result.ideResult.success) {
            const errorDetails = result.ideResult.details
                .filter(d => d.severity === 'error')
                .map(d => `  ${d.file}:${d.line}: ${d.message}`)
                .join('\n');
            details.push(`IDE 错误:\n${errorDetails}`);
        }
        if (result.lintResult && !result.lintResult.success) {
            details.push(`Lint 错误:\n${result.lintResult.error || result.lintResult.output}`);
        }
        if (result.testResult && !result.testResult.success) {
            details.push(`测试错误:\n${result.testResult.error || result.testResult.output}`);
        }
        return details.join('\n\n');
    }
}
exports.VerificationRunner = VerificationRunner;
//# sourceMappingURL=verification-runner.js.map
/**
 * 验证执行器
 * 负责执行 Phase 4 的验证检查：编译、Lint、测试、IDE 诊断
 */

import { spawn } from 'child_process';
import * as vscode from 'vscode';
import { globalEventBus } from '../events';

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

const DEFAULT_CONFIG: VerificationConfig = {
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
export class VerificationRunner {
  private config: VerificationConfig;
  private workspaceRoot: string;

  constructor(workspaceRoot: string, config?: Partial<VerificationConfig>) {
    this.workspaceRoot = workspaceRoot;
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /** 更新配置 */
  updateConfig(config: Partial<VerificationConfig>): void {
    this.config = { ...this.config, ...config };
  }

  /**
   * 执行完整验证流程
   */
  async runVerification(taskId: string, modifiedFiles?: string[]): Promise<VerificationResult> {
    console.log('[VerificationRunner] 开始验证检查...');
    globalEventBus.emitEvent('verification:started', { taskId });

    const result: VerificationResult = {
      success: true,
      summary: '',
    };

    const summaryParts: string[] = [];

    // 1. 编译检查
    if (this.config.compileCheck) {
      console.log('[VerificationRunner] 执行编译检查...');
      result.compileResult = await this.runCommand(this.config.compileCommand, '编译');
      if (!result.compileResult.success) {
        result.success = false;
        summaryParts.push(`❌ 编译失败: ${result.compileResult.error || '未知错误'}`);
      } else {
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
      } else {
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
      } else {
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
      } else {
        summaryParts.push('✅ 测试通过');
      }
    }

    result.summary = summaryParts.join(' | ');
    
    globalEventBus.emitEvent('verification:completed', { 
      taskId, 
      data: { success: result.success, summary: result.summary } 
    });

    console.log(`[VerificationRunner] 验证完成: ${result.success ? '通过' : '失败'}`);
    return result;
  }

  /**
   * 执行命令并返回结果
   */
  private async runCommand(command: string, name: string): Promise<CommandResult> {
    const startTime = Date.now();

    return new Promise((resolve) => {
      const [cmd, ...args] = command.split(' ');
      const process = spawn(cmd, args, {
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
  private async runIDEDiagnostics(modifiedFiles?: string[]): Promise<IDEDiagnosticResult> {
    const result: IDEDiagnosticResult = {
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
          const isModified = modifiedFiles.some(f =>
            filePath.endsWith(f) || filePath.includes(f)
          );
          if (!isModified) continue;
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
          } else if (diagnostic.severity === vscode.DiagnosticSeverity.Warning) {
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
    } catch (error) {
      console.error('[VerificationRunner] IDE 诊断检查失败:', error);
      result.success = false;
    }

    return result;
  }

  /**
   * 快速编译检查
   */
  async quickCompileCheck(): Promise<boolean> {
    if (!this.config.compileCheck) return true;
    const result = await this.runCommand(this.config.compileCommand, '编译');
    return result.success;
  }

  /**
   * 获取错误详情（用于恢复阶段）
   */
  getErrorDetails(result: VerificationResult): string {
    const details: string[] = [];

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


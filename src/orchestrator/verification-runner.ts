/**
 * 验证执行器
 * 负责执行 Phase 4 的验证检查：编译、Lint、测试、IDE 诊断
 */

import { logger, LogCategory } from '../logging';
import { spawn } from 'child_process';
import * as vscode from 'vscode';
import { globalEventBus } from '../events';
import * as fs from 'fs';
import * as path from 'path';

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
    logger.info('编排器.验证.开始', { taskId }, LogCategory.ORCHESTRATOR);
    globalEventBus.emitEvent('verification:started', { taskId });

    const verificationRoots = this.resolveVerificationRoots(modifiedFiles);

    const result: VerificationResult = {
      success: true,
      summary: '',
    };

    const summaryParts: string[] = [];

    // 1. 编译检查
    if (this.config.compileCheck) {
      logger.info('编排器.验证.编译.开始', { taskId }, LogCategory.ORCHESTRATOR);
      result.compileResult = await this.runCompileChecks(verificationRoots);
      if (!result.compileResult.success) {
        result.success = false;
        summaryParts.push(`编译失败: ${result.compileResult.error || '未知错误'}`);
      } else {
        summaryParts.push(verificationRoots.length > 1 ? `编译通过（${verificationRoots.length} 个项目）` : '编译通过');
      }
    }

    // 2. IDE 诊断检查
    if (this.config.ideCheck) {
      logger.info('编排器.验证.IDE.开始', { taskId }, LogCategory.ORCHESTRATOR);
      result.ideResult = await this.runIDEDiagnostics(modifiedFiles);
      if (!result.ideResult.success) {
        result.success = false;
        summaryParts.push(`IDE 诊断: ${result.ideResult.errors} 个错误`);
      } else {
        const warningText = result.ideResult.warnings > 0 
          ? ` (${result.ideResult.warnings} 个警告)` 
          : '';
        summaryParts.push(`IDE 诊断通过${warningText}`);
      }
    }

    // 3. Lint 检查
    if (this.config.lintCheck) {
      logger.info('编排器.验证.Lint.开始', { taskId }, LogCategory.ORCHESTRATOR);
      result.lintResult = await this.runLintChecks(verificationRoots);
      if (!result.lintResult.success) {
        result.success = false;
        summaryParts.push(`Lint 失败: ${result.lintResult.error || '未知错误'}`);
      } else {
        summaryParts.push(verificationRoots.length > 1 ? `Lint 通过（${verificationRoots.length} 个项目）` : 'Lint 通过');
      }
    }

    // 4. 测试检查
    if (this.config.testCheck) {
      logger.info('编排器.验证.测试.开始', { taskId }, LogCategory.ORCHESTRATOR);
      result.testResult = await this.runTestChecks(verificationRoots);
      if (!result.testResult.success) {
        result.success = false;
        summaryParts.push(`测试失败: ${result.testResult.error || '未知错误'}`);
      } else {
        summaryParts.push(verificationRoots.length > 1 ? `测试通过（${verificationRoots.length} 个项目）` : '测试通过');
      }
    }

    result.summary = summaryParts.join(' | ');
    
    globalEventBus.emitEvent('verification:completed', { 
      taskId, 
      data: { success: result.success, summary: result.summary } 
    });

    logger.info('编排器.验证.完成', { taskId, success: result.success }, LogCategory.ORCHESTRATOR);
    return result;
  }

  /**
   * 执行命令并返回结果
   */
  private async runCommand(command: string, name: string, cwd?: string): Promise<CommandResult> {
    const startTime = Date.now();
    const executionCwd = cwd || this.workspaceRoot;

    return new Promise((resolve) => {
      const process = spawn(command, {
        cwd: executionCwd,
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
      logger.error('编排器.验证.IDE.失败', error, LogCategory.ORCHESTRATOR);
      result.success = false;
    }

    return result;
  }

  /**
   * 快速编译检查
   */
  async quickCompileCheck(): Promise<boolean> {
    if (!this.config.compileCheck) return true;
    const compileCommand = this.resolveCompileCommand(this.workspaceRoot);
    if (!compileCommand) return false;
    const result = await this.runCommand(compileCommand, '编译', this.workspaceRoot);
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

  private resolveVerificationRoots(modifiedFiles?: string[]): string[] {
    const hitCount = new Map<string, number>();
    if (Array.isArray(modifiedFiles)) {
      for (const file of modifiedFiles) {
        const normalized = this.normalizeModifiedPath(file);
        if (!normalized) continue;
        const projectRoot = this.findNearestProjectRoot(normalized) || this.workspaceRoot;
        hitCount.set(projectRoot, (hitCount.get(projectRoot) || 0) + 1);
      }
    }

    if (hitCount.size === 0) {
      return [this.workspaceRoot];
    }

    return Array.from(hitCount.entries())
      .sort((a, b) => {
        if (b[1] !== a[1]) return b[1] - a[1];
        return b[0].length - a[0].length;
      })
      .map(([root]) => root);
  }

  private async runCompileChecks(projectRoots: string[]): Promise<CommandResult> {
    return this.runProjectChecks({
      projectRoots,
      name: '编译',
      resolveCommand: (root) => this.resolveCompileCommand(root),
      missingCommandMessage: '未找到可用编译命令（缺少 scripts.compile/scripts.typecheck 与 tsconfig.json）',
    });
  }

  private async runLintChecks(projectRoots: string[]): Promise<CommandResult> {
    return this.runProjectChecks({
      projectRoots,
      name: 'Lint',
      resolveCommand: (root) => this.resolveLintCommand(root),
      missingCommandMessage: '未找到可用 Lint 命令（缺少 scripts.lint）',
    });
  }

  private async runTestChecks(projectRoots: string[]): Promise<CommandResult> {
    return this.runProjectChecks({
      projectRoots,
      name: '测试',
      resolveCommand: (root) => this.resolveTestCommand(root),
      missingCommandMessage: '未找到可用测试命令（缺少 scripts.test）',
    });
  }

  private async runProjectChecks(params: {
    projectRoots: string[];
    name: string;
    resolveCommand: (root: string) => string | null;
    missingCommandMessage: string;
  }): Promise<CommandResult> {
    const { projectRoots, name, resolveCommand, missingCommandMessage } = params;
    const roots = projectRoots.length > 0 ? projectRoots : [this.workspaceRoot];
    const outputs: string[] = [];
    const errors: string[] = [];
    let duration = 0;
    let success = true;

    for (const root of roots) {
      const command = resolveCommand(root);
      if (!command) {
        success = false;
        errors.push(`[${root}] ${missingCommandMessage}`);
        continue;
      }

      const commandResult = await this.runCommand(command, name, root);
      duration += commandResult.duration;

      if (commandResult.output?.trim()) {
        outputs.push(`[${root}]\n${commandResult.output.trim()}`);
      }

      if (!commandResult.success) {
        success = false;
        const errorText = commandResult.error?.trim() || '未知错误';
        errors.push(`[${root}] ${errorText}`);
      }
    }

    return {
      success,
      output: outputs.join('\n\n'),
      error: errors.length > 0 ? errors.join('\n\n') : undefined,
      duration,
    };
  }

  private normalizeModifiedPath(file: string): string | null {
    if (!file || typeof file !== 'string') {
      return null;
    }
    const trimmed = file.trim();
    if (!trimmed) {
      return null;
    }
    if (path.isAbsolute(trimmed)) {
      return path.resolve(trimmed);
    }

    const workspaceFolders = vscode.workspace.workspaceFolders || [];
    const separatorIndex = trimmed.indexOf('/');
    if (separatorIndex > 0) {
      const workspaceName = trimmed.slice(0, separatorIndex);
      const relativePath = trimmed.slice(separatorIndex + 1);
      const matchedFolder = workspaceFolders.find(folder => folder.name === workspaceName);
      if (matchedFolder) {
        return path.resolve(matchedFolder.uri.fsPath, relativePath);
      }
    }

    for (const folder of workspaceFolders) {
      const candidate = path.resolve(folder.uri.fsPath, trimmed);
      if (fs.existsSync(candidate)) {
        return candidate;
      }
    }

    return path.resolve(this.workspaceRoot, trimmed);
  }

  private findNearestProjectRoot(filePath: string): string | null {
    let current = filePath;
    try {
      if (fs.existsSync(filePath) && fs.statSync(filePath).isFile()) {
        current = path.dirname(filePath);
      }
    } catch {
      current = path.dirname(filePath);
    }

    const workspaceRootResolved = path.resolve(this.workspaceRoot);
    while (true) {
      const packageJsonPath = path.join(current, 'package.json');
      const tsconfigPath = path.join(current, 'tsconfig.json');
      if (fs.existsSync(packageJsonPath) || fs.existsSync(tsconfigPath)) {
        return current;
      }
      if (current === workspaceRootResolved) {
        break;
      }
      const parent = path.dirname(current);
      if (parent === current) {
        break;
      }
      current = parent;
    }

    return null;
  }

  private resolveCompileCommand(cwd: string): string | null {
    const configuredCommand = this.config.compileCommand?.trim();
    if (configuredCommand && configuredCommand !== DEFAULT_CONFIG.compileCommand) {
      return configuredCommand;
    }

    const scripts = this.readPackageScripts(cwd);
    if (scripts.compile) return 'npm run compile';
    if (scripts.typecheck) return 'npm run typecheck';

    const tsconfigPath = path.join(cwd, 'tsconfig.json');
    if (fs.existsSync(tsconfigPath)) {
      return `npx tsc --noEmit -p \"${tsconfigPath}\"`;
    }

    return null;
  }

  private resolveLintCommand(cwd: string): string | null {
    const configuredCommand = this.config.lintCommand?.trim();
    if (configuredCommand && configuredCommand !== DEFAULT_CONFIG.lintCommand) {
      return configuredCommand;
    }

    const scripts = this.readPackageScripts(cwd);
    return scripts.lint ? 'npm run lint' : null;
  }

  private resolveTestCommand(cwd: string): string | null {
    const configuredCommand = this.config.testCommand?.trim();
    if (configuredCommand && configuredCommand !== DEFAULT_CONFIG.testCommand) {
      return configuredCommand;
    }

    const scripts = this.readPackageScripts(cwd);
    return scripts.test ? 'npm test' : null;
  }

  private readPackageScripts(cwd: string): Record<string, string> {
    try {
      const packageJsonPath = path.join(cwd, 'package.json');
      if (!fs.existsSync(packageJsonPath)) {
        return {};
      }
      const content = fs.readFileSync(packageJsonPath, 'utf-8');
      const parsed = JSON.parse(content) as { scripts?: Record<string, string> };
      return parsed.scripts && typeof parsed.scripts === 'object' ? parsed.scripts : {};
    } catch {
      return {};
    }
  }
}

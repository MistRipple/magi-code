/**
 * 验证执行器
 * 负责执行 Phase 4 的验证检查：编译、Lint、测试、IDE 诊断
 */

import { logger, LogCategory } from '../logging';
import { spawn } from 'child_process';
import { globalEventBus } from '../events';
import * as fs from 'fs';
import * as path from 'path';
import type { DiagnosticsHost } from '../host';
import type { WorkspaceFolderInfo } from '../workspace/workspace-roots';

/** 验证配置 */
export interface VerificationConfig {
  /** 编译检查（默认 true） */
  compileCheck: boolean;
  /** 编译命令（默认 npm run compile） */
  compileCommand: string;
  /** 缺少编译命令时的策略（默认 warn） */
  compileMissingCommandPolicy: 'warn' | 'fail';
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
  warnings?: string[];
  summary: string;
}

/** 命令执行结果 */
export interface CommandResult {
  success: boolean;
  output: string;
  error?: string;
  warnings?: string[];
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
  compileMissingCommandPolicy: 'warn',
  ideCheck: true,
  lintCheck: false,
  lintCommand: 'npm run lint',
  testCheck: false,
  testCommand: 'npm test',
  timeout: 60000,
};

const NON_BLOCKING_WARNING_PATTERNS: RegExp[] = [
  /自动跳过编译检查/i,
  /未找到可用编译命令（缺少 scripts\.compile\/scripts\.typecheck 与 tsconfig\.json）/i,
  /missing scripts\.compile\/scripts\.typecheck.*tsconfig\.json/i,
];

export function isNonBlockingVerificationWarning(warning: string): boolean {
  const normalized = typeof warning === 'string' ? warning.trim() : '';
  if (!normalized) return false;
  return NON_BLOCKING_WARNING_PATTERNS.some((pattern) => pattern.test(normalized));
}

/**
 * 验证执行器
 */
export class VerificationRunner {
  private config: VerificationConfig;
  private workspaceRoot: string;
  private readonly diagnosticsHost?: DiagnosticsHost;
  private readonly workspaceFolders: WorkspaceFolderInfo[];

  constructor(
    workspaceRoot: string,
    diagnosticsHost?: DiagnosticsHost,
    config?: Partial<VerificationConfig>,
    workspaceFolders: WorkspaceFolderInfo[] = [],
  ) {
    this.workspaceRoot = workspaceRoot;
    this.diagnosticsHost = diagnosticsHost;
    this.workspaceFolders = [...workspaceFolders];
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
      warnings: [],
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
      if (result.compileResult.warnings && result.compileResult.warnings.length > 0) {
        result.warnings?.push(...result.compileResult.warnings);
        const userFacingWarnings = result.compileResult.warnings.filter((item) => !isNonBlockingVerificationWarning(item));
        if (userFacingWarnings.length > 0) {
          summaryParts.push(`编译告警: ${userFacingWarnings.join('；')}`);
        }
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
      const diagnostics = this.diagnosticsHost
        ? await this.diagnosticsHost.getDiagnostics()
        : [];

      for (const diagnostic of diagnostics) {
        if (modifiedFiles && modifiedFiles.length > 0) {
          const isModified = modifiedFiles.some((file) =>
            diagnostic.file.endsWith(file) || diagnostic.file.includes(file));
          if (!isModified) {
            continue;
          }
        }

        if (diagnostic.severity === 'error') {
          result.errors++;
        } else if (diagnostic.severity === 'warning') {
          result.warnings++;
        }

        result.details.push({
          file: diagnostic.file,
          line: diagnostic.line,
          message: diagnostic.message,
          severity: diagnostic.severity,
        });
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
    if (!compileCommand) {
      return this.config.compileMissingCommandPolicy === 'warn';
    }
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
      missingCommandMessage: '自动跳过编译检查：未检测到 scripts.compile/scripts.typecheck 与 tsconfig.json（不阻断执行）',
      missingCommandPolicy: this.config.compileMissingCommandPolicy,
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
    missingCommandPolicy?: 'warn' | 'fail';
  }): Promise<CommandResult> {
    const { projectRoots, name, resolveCommand, missingCommandMessage, missingCommandPolicy = 'fail' } = params;
    const roots = projectRoots.length > 0 ? projectRoots : [this.workspaceRoot];
    const outputs: string[] = [];
    const errors: string[] = [];
    const warnings: string[] = [];
    let duration = 0;
    let success = true;

    for (const root of roots) {
      const command = resolveCommand(root);
      if (!command) {
        const missingMessage = `[${root}] ${missingCommandMessage}`;
        if (missingCommandPolicy === 'warn') {
          warnings.push(missingMessage);
        } else {
          success = false;
          errors.push(missingMessage);
        }
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
      warnings: warnings.length > 0 ? warnings : undefined,
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

    const workspaceFolders = this.workspaceFolders;
    const separatorIndex = trimmed.indexOf('/');
    if (separatorIndex > 0) {
      const workspaceName = trimmed.slice(0, separatorIndex);
      const relativePath = trimmed.slice(separatorIndex + 1);
      const matchedFolder = workspaceFolders.find((folder) => folder.name === workspaceName);
      if (matchedFolder) {
        return path.resolve(matchedFolder.path, relativePath);
      }
    }

    for (const folder of workspaceFolders) {
      const candidate = path.resolve(folder.path, trimmed);
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
    if (scripts.check) return 'npm run check';

    const tsconfigPath = path.join(cwd, 'tsconfig.json');
    if (fs.existsSync(tsconfigPath)) {
      return `npx tsc --noEmit -p \"${tsconfigPath}\"`;
    }

    // 自动处理：未定义 compile/typecheck，但存在 build 时直接复用 build 验证
    if (scripts.build) return 'npm run build';

    // 自动处理：TypeScript 项目缺少 tsconfig/scripts 时，使用临时类型检查命令继续推进
    const transientTypecheck = this.resolveTransientTypecheckCommand(cwd);
    if (transientTypecheck) {
      return transientTypecheck;
    }

    return null;
  }

  private resolveTransientTypecheckCommand(cwd: string): string | null {
    if (!this.hasTypeScriptSource(cwd)) {
      return null;
    }
    // 不修改用户工程文件：通过 npx 临时拉起 tsc 做 noEmit 校验
    return 'npx --yes -p typescript tsc --noEmit --skipLibCheck --pretty false --jsx preserve';
  }

  private hasTypeScriptSource(cwd: string): boolean {
    const maxVisitedDirs = 800;
    const skipDirs = new Set([
      'node_modules',
      '.git',
      '.hg',
      '.svn',
      'dist',
      'build',
      'out',
      '.next',
      '.nuxt',
      '.cache',
      'coverage',
    ]);
    const tsExtensions = ['.ts', '.tsx', '.mts', '.cts'];
    const queue: string[] = [cwd];
    let visited = 0;

    while (queue.length > 0 && visited < maxVisitedDirs) {
      const current = queue.pop() as string;
      visited += 1;
      let entries: fs.Dirent[] = [];
      try {
        entries = fs.readdirSync(current, { withFileTypes: true });
      } catch {
        continue;
      }

      for (const entry of entries) {
        const entryPath = path.join(current, entry.name);
        if (entry.isDirectory()) {
          if (skipDirs.has(entry.name)) continue;
          queue.push(entryPath);
          continue;
        }
        if (!entry.isFile()) continue;
        const lower = entry.name.toLowerCase();
        if (tsExtensions.some((ext) => lower.endsWith(ext))) {
          return true;
        }
      }
    }
    return false;
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
    } catch (readError) {
      logger.warn('验证器.package.json解析失败', {
        cwd,
        error: readError instanceof Error ? readError.message : String(readError),
      }, LogCategory.ORCHESTRATOR);
      return {};
    }
  }
}

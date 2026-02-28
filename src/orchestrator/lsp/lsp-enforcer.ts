/**
 * LSP 强制执行器
 * 在满足条件时自动触发 LSP 预检，并注入到 Worker 引导中
 */

import * as path from 'path';
import { Assignment } from '../mission';
import { ToolCall } from '../../llm/types';
import { logger, LogCategory } from '../../logging';
import type { ToolManager } from '../../tools/tool-manager';

interface LspPreflightSummary {
  enforced: boolean;
  reason: string;
  targets: string[];
  diagnostics: string[];
  symbols: string[];
}

const SUPPORTED_EXTS = new Set([
  '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs', '.py'
]);

const CATEGORY_FORCE = new Set([
  'refactor',
  'bugfix',
  'debug',
  'integration',
  'implement',
  'implementation'
]);

const RESPONSIBILITY_KEYWORDS = [
  'refactor',
  'bug',
  'fix',
  'error',
  'issue',
  'type',
  'compile',
  'lint',
  '依赖',
  '引用',
  '类型',
  '报错',
  '修复',
  '重构',
  '编译',
  '诊断'
];

const MAX_FILES = 4;
const MAX_DIAGNOSTICS = 8;
const MAX_SYMBOLS = 12;

export class LspEnforcer {
  private workspaceRoot: string;
  private toolManager: ToolManager;
  /** applyIfNeeded 中 runPreflight 产生的 diagnostics 缓存，供 captureDiagnostics 直接复用 */
  private cachedPreflightDiagnostics: string[] | null = null;

  constructor(workspaceRoot: string, toolManager: ToolManager) {
    this.workspaceRoot = workspaceRoot;
    this.toolManager = toolManager;
  }

  async applyIfNeeded(assignment: Assignment): Promise<boolean> {
    // 每次调用前清除上一轮缓存
    this.cachedPreflightDiagnostics = null;

    if (this.hasLspInjected(assignment)) {
      return false;
    }

    const targetFiles = this.collectTargetFiles(assignment);
    const supportedTargets = targetFiles.filter((file) => this.isSupportedFile(file));

    if (supportedTargets.length === 0) {
      return false;
    }

    const reason = this.buildReason(assignment, supportedTargets);
    if (!reason) {
      return false;
    }

    const summary = await this.runPreflight(assignment, supportedTargets, reason);
    if (!summary) {
      return false;
    }

    // 缓存 runPreflight 中已查询到的 diagnostics，供后续 captureDiagnostics 复用
    this.cachedPreflightDiagnostics = summary.diagnostics;

    assignment.guidancePrompt = this.injectGuidance(assignment.guidancePrompt, summary);
    logger.info('LSP 预检已注入', {
      assignmentId: assignment.id,
      reason: summary.reason,
      targets: summary.targets.length
    }, LogCategory.ORCHESTRATOR);
    return true;
  }

  /**
   * 获取目标文件的当前诊断快照（供 postCheck 对比用）
   * 优先复用 applyIfNeeded → runPreflight 中已缓存的结果，避免重复 LSP 查询
   */
  async captureDiagnostics(assignment: Assignment): Promise<string[]> {
    if (this.cachedPreflightDiagnostics !== null) {
      const cached = this.cachedPreflightDiagnostics;
      this.cachedPreflightDiagnostics = null; // 一次性消费
      return cached;
    }

    const targetFiles = this.collectTargetFiles(assignment);
    const supportedTargets = targetFiles.filter((file) => this.isSupportedFile(file)).slice(0, MAX_FILES);
    const diagnostics: string[] = [];

    for (const target of supportedTargets) {
      const diag = await this.queryLsp('diagnostics', target);
      if (diag.ok) {
        diagnostics.push(...this.formatDiagnostics(target, diag.data));
      }
    }

    return diagnostics;
  }

  /**
   * 执行后诊断验证：对比执行前后的诊断列表，找出新增项
   */
  async postCheck(
    assignment: Assignment,
    preflightDiagnostics: string[]
  ): Promise<{ newErrors: string[] } | null> {
    try {
      const currentDiagnostics = await this.captureDiagnostics(assignment);
      const preSet = new Set(preflightDiagnostics);
      const newErrors = currentDiagnostics.filter(
        (d) => !preSet.has(d) && d.includes('[Error]')
      );

      if (newErrors.length === 0) {
        return null;
      }

      logger.warn('LSP 后检发现新增编译错误', {
        assignmentId: assignment.id,
        newErrorCount: newErrors.length
      }, LogCategory.ORCHESTRATOR);

      return { newErrors: newErrors.slice(0, MAX_DIAGNOSTICS) };
    } catch (error: any) {
      logger.warn('LSP 后检失败，跳过', {
        assignmentId: assignment.id,
        error: error?.message
      }, LogCategory.ORCHESTRATOR);
      return null;
    }
  }

  private hasLspInjected(assignment: Assignment): boolean {
    return assignment.guidancePrompt.includes('## LSP 预检');
  }

  private collectTargetFiles(assignment: Assignment): string[] {
    const files = new Set<string>();
    if (assignment.scope?.targetPaths) {
      assignment.scope.targetPaths.forEach((p) => files.add(p));
    }
    if (assignment.scope?.includes) {
      assignment.scope.includes.forEach((p) => files.add(p));
    }
    return Array.from(files);
  }

  private isSupportedFile(filePath: string): boolean {
    const ext = path.extname(filePath);
    return SUPPORTED_EXTS.has(ext);
  }

  private buildReason(assignment: Assignment, targets: string[]): string | null {
    const reasons: string[] = [];
    if (targets.length >= 2) {
      reasons.push('多文件任务');
    }

    const category = assignment.assignmentReason?.profileMatch?.category?.toLowerCase();
    if (category && CATEGORY_FORCE.has(category)) {
      reasons.push(`任务分类:${category}`);
    }

    const responsibility = assignment.responsibility || '';
    const lower = responsibility.toLowerCase();
    const keywordHit = RESPONSIBILITY_KEYWORDS.some((keyword) => lower.includes(keyword) || responsibility.includes(keyword));
    if (keywordHit) {
      reasons.push('职责包含诊断/引用/重构类关键词');
    }

    if (reasons.length === 0) {
      return null;
    }

    return reasons.join('；');
  }

  private async runPreflight(
    assignment: Assignment,
    targets: string[],
    reason: string
  ): Promise<LspPreflightSummary | null> {
    const selectedTargets = targets.slice(0, MAX_FILES);
    const diagnostics: string[] = [];
    const symbols: string[] = [];

    for (const target of selectedTargets) {
      const diag = await this.queryLsp('diagnostics', target);
      if (diag.ok) {
        diagnostics.push(...this.formatDiagnostics(target, diag.data));
      } else if (diag.error) {
        diagnostics.push(`${this.relativePath(target)}: 无法获取诊断 (${diag.error})`);
      }
    }

    for (const target of selectedTargets) {
      const sym = await this.queryLsp('documentSymbols', target);
      if (sym.ok) {
        const formatted = this.formatSymbols(target, sym.data);
        if (formatted.length > 0) {
          symbols.push(...formatted);
        }
      }
    }

    if (diagnostics.length === 0 && symbols.length === 0) {
      return null;
    }

    return {
      enforced: true,
      reason,
      targets: selectedTargets,
      diagnostics: diagnostics.slice(0, MAX_DIAGNOSTICS),
      symbols: symbols.slice(0, MAX_SYMBOLS)
    };
  }

  private async queryLsp(action: string, filePath: string): Promise<{ ok: boolean; data?: any; error?: string }> {
    const toolCall: ToolCall = {
      id: `lsp_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
      name: 'lsp_query',
      arguments: {
        action,
        filePath
      }
    };

    const result = await this.toolManager.execute(
      toolCall,
      undefined,
      { workerId: 'orchestrator', role: 'orchestrator' }
    );
    if (result.isError) {
      return { ok: false, error: String(result.content || 'LSP error') };
    }

    try {
      const parsed = JSON.parse(result.content);
      return { ok: true, data: parsed };
    } catch (error: any) {
      return { ok: false, error: error?.message || 'Invalid LSP response' };
    }
  }

  private formatDiagnostics(filePath: string, payload: any): string[] {
    const diagnostics = payload?.diagnostics;
    if (!Array.isArray(diagnostics)) {
      return [];
    }
    if (diagnostics.length === 0) {
      return [`${this.relativePath(filePath)}: 无诊断`];
    }
    return diagnostics.slice(0, MAX_DIAGNOSTICS).map((diag: any) => {
      const severity = this.mapSeverity(diag.severity);
      const range = diag.range?.start
        ? `${diag.range.start.line + 1}:${diag.range.start.character + 1}`
        : '?:?';
      return `${this.relativePath(filePath)}:${range} [${severity}] ${diag.message || '诊断'}`;
    });
  }

  private formatSymbols(filePath: string, payload: any): string[] {
    const symbols = payload?.symbols;
    if (!Array.isArray(symbols) || symbols.length === 0) {
      return [];
    }
    const names = symbols
      .map((sym: any) => sym?.name)
      .filter((name: any) => typeof name === 'string' && name.length > 0)
      .slice(0, MAX_SYMBOLS);
    if (names.length === 0) {
      return [];
    }
    return [`${this.relativePath(filePath)}: ${names.join(', ')}`];
  }

  private mapSeverity(severity: number | undefined): string {
    switch (severity) {
      case 0:
        return 'Error';
      case 1:
        return 'Warning';
      case 2:
        return 'Info';
      case 3:
        return 'Hint';
      default:
        return 'Unknown';
    }
  }

  private relativePath(filePath: string): string {
    if (!this.workspaceRoot) return filePath;
    if (!path.isAbsolute(filePath)) {
      return filePath;
    }
    const relative = path.relative(this.workspaceRoot, filePath);
    return relative || filePath;
  }

  private injectGuidance(original: string, summary: LspPreflightSummary): string {
    const sections: string[] = [];
    sections.push('## LSP 预检（强制）');
    sections.push(`触发原因: ${summary.reason}`);
    sections.push(`目标文件: ${summary.targets.map((f) => this.relativePath(f)).join(', ')}`);
    if (summary.diagnostics.length > 0) {
      sections.push('诊断摘要:');
      summary.diagnostics.forEach((line) => sections.push(`- ${line}`));
    }
    if (summary.symbols.length > 0) {
      sections.push('符号摘要:');
      summary.symbols.forEach((line) => sections.push(`- ${line}`));
    }
    sections.push('要求: 在修改前先确认以上诊断与符号信息，确保修改不会引入新的编译错误。');

    const injected = sections.join('\n');
    return original ? `${original}\n\n${injected}` : injected;
  }
}

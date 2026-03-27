import { spawn } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type {
  AcceptanceCriterion,
  AcceptanceCriterionExecutionReport,
  VerificationSpecType,
} from '../mission/types';
import type { CommandResult, VerificationRunner } from '../verification-runner';
import type { DispatchBatch } from './dispatch/dispatch-batch';

export interface VerificationCommandOptions {
  cwd?: string;
  timeoutMs?: number;
}

export interface VerificationCommandRunner {
  (command: string, options?: VerificationCommandOptions): Promise<CommandResult>;
}

export interface VerificationCustomValidatorResult {
  passed: boolean;
  detail: string;
}

export interface VerificationCustomValidator {
  (
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): Promise<VerificationCustomValidatorResult> | VerificationCustomValidatorResult;
}

export interface VerificationSpecExecutionContext {
  workspaceRoot: string;
  batch?: DispatchBatch;
  modifiedFiles?: string[];
  verificationRunner?: VerificationRunner;
  runCommand?: VerificationCommandRunner;
  customValidators?: Record<string, VerificationCustomValidator>;
}

export type VerificationSpecExecutionResult = AcceptanceCriterionExecutionReport;

export interface VerificationSpecExecutor {
  readonly type: VerificationSpecType;
  readonly executorId: string;
  execute(
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): Promise<VerificationSpecExecutionResult> | VerificationSpecExecutionResult;
}

function buildFailureResult(
  criterion: AcceptanceCriterion,
  type: VerificationSpecType,
  detail: string,
  executorId: string,
): VerificationSpecExecutionResult {
  return {
    criterionId: criterion.id,
    type,
    status: 'failed',
    detail,
    executorId,
  };
}

class FileExistsVerificationExecutor implements VerificationSpecExecutor {
  readonly type = 'file_exists' as const;
  readonly executorId = 'builtin:file_exists';

  execute(
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): VerificationSpecExecutionResult {
    const spec = criterion.verificationSpec;
    if (!spec?.targetPath) {
      return buildFailureResult(criterion, this.type, 'file_exists: targetPath 未指定', this.executorId);
    }

    const fullPath = path.resolve(context.workspaceRoot, spec.targetPath);
    const exists = fs.existsSync(fullPath);
    return {
      criterionId: criterion.id,
      type: this.type,
      status: exists ? 'passed' : 'failed',
      detail: exists ? `文件存在: ${spec.targetPath}` : `文件不存在: ${spec.targetPath}`,
      executorId: this.executorId,
    };
  }
}

class FileContentVerificationExecutor implements VerificationSpecExecutor {
  readonly type = 'file_content' as const;
  readonly executorId = 'builtin:file_content';

  execute(
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): VerificationSpecExecutionResult {
    const spec = criterion.verificationSpec;
    if (!spec?.targetPath || !spec.expectedContent) {
      return buildFailureResult(
        criterion,
        this.type,
        'file_content: targetPath 或 expectedContent 未指定',
        this.executorId,
      );
    }

    const filePath = path.resolve(context.workspaceRoot, spec.targetPath);
    if (!fs.existsSync(filePath)) {
      return buildFailureResult(criterion, this.type, `文件不存在: ${spec.targetPath}`, this.executorId);
    }

    const content = fs.readFileSync(filePath, 'utf-8');
    const mode = spec.contentMatchMode || 'contains';
    let matched = false;

    if (mode === 'exact') {
      matched = content === spec.expectedContent;
    } else if (mode === 'contains') {
      matched = content.includes(spec.expectedContent);
    } else {
      matched = new RegExp(spec.expectedContent).test(content);
    }

    return {
      criterionId: criterion.id,
      type: this.type,
      status: matched ? 'passed' : 'failed',
      detail: matched
        ? `文件内容匹配(${mode}): ${spec.targetPath}`
        : `文件内容不匹配(${mode}): ${spec.targetPath}`,
      executorId: this.executorId,
    };
  }
}

class TestPassVerificationExecutor implements VerificationSpecExecutor {
  readonly type = 'test_pass' as const;
  readonly executorId = 'builtin:test_pass';

  async execute(
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): Promise<VerificationSpecExecutionResult> {
    const spec = criterion.verificationSpec;
    const command = spec?.testCommand?.trim();
    if (!command) {
      return buildFailureResult(criterion, this.type, 'test_pass: testCommand 未指定', this.executorId);
    }
    if (!context.runCommand) {
      return buildFailureResult(criterion, this.type, 'test_pass: runCommand 未提供', this.executorId);
    }

    const result = await context.runCommand(command, { cwd: context.workspaceRoot });
    const failureOutput = compactCommandOutput(result.error || result.output);
    return {
      criterionId: criterion.id,
      type: this.type,
      status: result.success ? 'passed' : 'failed',
      detail: result.success
        ? `测试通过: ${command}`
        : `测试失败: ${command}${failureOutput ? ` | ${failureOutput}` : ''}`,
      executorId: this.executorId,
    };
  }
}

class TaskCompletedVerificationExecutor implements VerificationSpecExecutor {
  readonly type = 'task_completed' as const;
  readonly executorId = 'builtin:task_completed';

  execute(
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): VerificationSpecExecutionResult {
    const pattern = criterion.verificationSpec?.taskPattern?.trim().toLowerCase();
    if (!pattern) {
      return buildFailureResult(criterion, this.type, 'task_completed: taskPattern 未指定', this.executorId);
    }
    if (!context.batch) {
      return buildFailureResult(criterion, this.type, 'task_completed: batch 上下文缺失', this.executorId);
    }

    const matches = context.batch.getEntries().filter((entry) => {
      const texts = [
        entry.taskId,
        entry.taskContract.taskTitle,
        entry.taskContract.requirementAnalysis.goal,
      ]
        .filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
        .map((value) => value.toLowerCase());
      return texts.some((text) => text.includes(pattern));
    });

    if (matches.length === 0) {
      return buildFailureResult(criterion, this.type, `未找到匹配任务: ${pattern}`, this.executorId);
    }

    const incomplete = matches.filter((entry) => entry.status !== 'completed' || entry.result?.success === false);
    if (incomplete.length > 0) {
      return buildFailureResult(
        criterion,
        this.type,
        `匹配任务未全部完成: ${incomplete.map((entry) => `${entry.taskId}=${entry.status}`).join(', ')}`,
        this.executorId,
      );
    }

    return {
      criterionId: criterion.id,
      type: this.type,
      status: 'passed',
      detail: `匹配任务已完成: ${matches.map((entry) => entry.taskId).join(', ')}`,
      executorId: this.executorId,
    };
  }
}

class CustomVerificationExecutor implements VerificationSpecExecutor {
  readonly type = 'custom' as const;
  readonly executorId = 'builtin:custom';

  async execute(
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): Promise<VerificationSpecExecutionResult> {
    const validatorName = criterion.verificationSpec?.customValidator?.trim();
    if (!validatorName) {
      return buildFailureResult(criterion, this.type, 'custom: customValidator 未指定', this.executorId);
    }

    const validator = context.customValidators?.[validatorName];
    if (!validator) {
      return buildFailureResult(criterion, this.type, `custom: 未注册校验器 ${validatorName}`, this.executorId);
    }

    const result = await validator(criterion, context);
    return {
      criterionId: criterion.id,
      type: this.type,
      status: result.passed ? 'passed' : 'failed',
      detail: result.detail,
      executorId: `custom:${validatorName}`,
    };
  }
}

function compactCommandOutput(text: string | undefined, limit: number = 240): string {
  const normalized = typeof text === 'string' ? text.trim().replace(/\s+/g, ' ') : '';
  if (!normalized) {
    return '';
  }
  return normalized.length > limit ? `${normalized.slice(0, limit)}...` : normalized;
}

export function createProcessVerificationCommandRunner(
  defaultOptions?: VerificationCommandOptions,
): VerificationCommandRunner {
  return (command: string, options?: VerificationCommandOptions) => new Promise((resolve) => {
    const startTime = Date.now();
    const cwd = options?.cwd || defaultOptions?.cwd;
    const timeout = options?.timeoutMs || defaultOptions?.timeoutMs || 60_000;
    const process = spawn(command, {
      cwd,
      shell: true,
      timeout,
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
      resolve({
        success: code === 0,
        output: stdout,
        error: code === 0 ? undefined : (stderr || stdout || `命令执行失败，退出码: ${code}`),
        duration: Date.now() - startTime,
      });
    });

    process.on('error', (error) => {
      resolve({
        success: false,
        output: stdout,
        error: error.message,
        duration: Date.now() - startTime,
      });
    });
  });
}

export class ValidatorRegistry {
  private readonly executors = new Map<VerificationSpecType, VerificationSpecExecutor>();

  register(executor: VerificationSpecExecutor, options?: { replace?: boolean }): void {
    const existing = this.executors.get(executor.type);
    if (existing && !options?.replace) {
      throw new Error(`验证器已注册: ${executor.type} -> ${existing.executorId}`);
    }
    this.executors.set(executor.type, executor);
  }

  get(type: VerificationSpecType): VerificationSpecExecutor | undefined {
    return this.executors.get(type);
  }

  async executeCriterion(
    criterion: AcceptanceCriterion,
    context: VerificationSpecExecutionContext,
  ): Promise<VerificationSpecExecutionResult | null> {
    if (!criterion.verifiable || !criterion.verificationSpec) {
      return null;
    }

    const executor = this.executors.get(criterion.verificationSpec.type);
    if (!executor) {
      return buildFailureResult(
        criterion,
        criterion.verificationSpec.type,
        `未知验证类型: ${criterion.verificationSpec.type}`,
        'registry:missing-executor',
      );
    }

    try {
      return await executor.execute(criterion, context);
    } catch (error: any) {
      return buildFailureResult(
        criterion,
        criterion.verificationSpec.type,
        `验证异常: ${error?.message || String(error)}`,
        executor.executorId,
      );
    }
  }

  async executeCriteria(
    criteria: AcceptanceCriterion[],
    context: VerificationSpecExecutionContext,
  ): Promise<VerificationSpecExecutionResult[]> {
    const results: VerificationSpecExecutionResult[] = [];
    for (const criterion of criteria) {
      const result = await this.executeCriterion(criterion, context);
      if (result) {
        results.push(result);
      }
    }
    return results;
  }
}

export function createDefaultValidatorRegistry(): ValidatorRegistry {
  const registry = new ValidatorRegistry();
  registry.register(new FileExistsVerificationExecutor());
  registry.register(new FileContentVerificationExecutor());
  registry.register(new TaskCompletedVerificationExecutor());
  registry.register(new TestPassVerificationExecutor());
  registry.register(new CustomVerificationExecutor());
  return registry;
}

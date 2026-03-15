import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../../logging';
import type { MessageHub } from './message-hub';
import type { VerificationRunner } from '../verification-runner';
import type { DispatchBatch } from './dispatch-batch';
import type { AcceptanceCriterion, VerificationSpec } from '../mission/types';

export type DeliveryVerificationStatus = 'skipped' | 'passed' | 'failed';

export interface DeliveryVerificationOutcome {
  status: DeliveryVerificationStatus;
  summary: string;
  details?: string;
  warnings?: string[];
  skippedReason?: 'no_runner' | 'no_entries' | 'execution_failed' | 'no_changes';
  /** 结构化验收标准的程序化验证结果 */
  specResults?: Array<{ criterionId: string; passed: boolean; detail: string }>;
}

function collectBatchModifiedFiles(batch: DispatchBatch): string[] {
  const modifiedFiles = new Set<string>();
  for (const entry of batch.getEntries()) {
    for (const file of entry.result?.modifiedFiles || []) {
      const normalized = file.trim();
      if (normalized) {
        modifiedFiles.add(normalized);
      }
    }
  }
  return Array.from(modifiedFiles);
}

export async function runPostDispatchVerification(
  batch: DispatchBatch,
  verificationRunner: VerificationRunner | undefined,
  messageHub: MessageHub,
  options?: {
    workspaceRoot?: string;
    acceptanceCriteria?: AcceptanceCriterion[];
  },
): Promise<DeliveryVerificationOutcome> {
  if (!verificationRunner) {
    return {
      status: 'skipped',
      summary: '未配置验收执行器',
      skippedReason: 'no_runner',
    };
  }

  const entries = batch.getEntries();
  if (entries.length === 0) {
    return {
      status: 'skipped',
      summary: '未发现可验收的任务',
      skippedReason: 'no_entries',
    };
  }

  const hasTerminalFailure = entries.some(entry =>
    entry.status === 'failed' || entry.status === 'cancelled'
  );
  if (hasTerminalFailure) {
    logger.info('编排器.验证.跳过', {
      reason: '存在失败或取消的子任务',
      batchId: batch.id,
    }, LogCategory.ORCHESTRATOR);
    return {
      status: 'skipped',
      summary: '子任务失败或取消，跳过验收',
      skippedReason: 'execution_failed',
    };
  }

  const modifiedFiles = collectBatchModifiedFiles(batch);
  if (modifiedFiles.length === 0) {
    logger.info('编排器.验证.跳过', {
      reason: '未检测到文件修改',
      batchId: batch.id,
    }, LogCategory.ORCHESTRATOR);
    return {
      status: 'skipped',
      summary: '未检测到文件修改，跳过验收',
      skippedReason: 'no_changes',
    };
  }

  messageHub.progress('Verification', `正在执行验收检查（${modifiedFiles.length} 个修改文件）...`);
  const verificationResult = await verificationRunner.runVerification(batch.id, modifiedFiles);

  // 程序化验证：对含有 verificationSpec 的 AcceptanceCriterion 进行结构化检查
  const specResults = options?.workspaceRoot && options?.acceptanceCriteria
    ? runSpecVerifications(options.acceptanceCriteria, options.workspaceRoot)
    : [];
  const specFailures = specResults.filter(r => !r.passed);
  const allWarnings = [...(verificationResult.warnings || [])];

  if (specFailures.length > 0) {
    const baseSummary = verificationResult.success
      ? (verificationResult.summary || '基础验收通过')
      : (verificationResult.summary || '基础验收失败');
    const baseDetailsRaw = verificationResult.success
      ? ''
      : verificationRunner.getErrorDetails(verificationResult).trim();
    const baseDetails = baseDetailsRaw.length > 0
      ? (baseDetailsRaw.length > 3000 ? `${baseDetailsRaw.slice(0, 3000)}\n...(错误详情已截断)` : baseDetailsRaw)
      : '';
    const mergedDetails = [
      `基础验收结论：${baseSummary}`,
      baseDetails ? `基础验收详情：\n${baseDetails}` : '',
      `结构化验收缺口：\n${specFailures.map(f => `- [${f.criterionId}] ${f.detail}`).join('\n')}`,
    ].filter(item => item.trim().length > 0).join('\n\n');
    const compactMergedDetails = mergedDetails.length > 3000
      ? `${mergedDetails.slice(0, 3000)}\n...(错误详情已截断)`
      : mergedDetails;
    allWarnings.push(...specFailures.map(f => `[${f.criterionId}] ${f.detail}`));
    messageHub.progress('Verification', `❌ 验收失败：结构化验收未通过（${specFailures.length} 项）`);
    if (allWarnings.length > 0) {
      messageHub.notify(`验收告警：${allWarnings.join('；')}`, 'warning');
    }
    return {
      status: 'failed',
      summary: `验收失败：结构化验收未通过（${specFailures.length} 项）`,
      details: compactMergedDetails,
      warnings: allWarnings.length > 0 ? allWarnings : undefined,
      specResults,
    };
  }

  if (verificationResult.success) {
    const passedSummary = verificationResult.summary || '验收通过';
    if (allWarnings.length > 0) {
      messageHub.notify(`验收告警：${allWarnings.join('；')}`, 'warning');
    }
    messageHub.progress('Verification', `✅ ${passedSummary}`);
    return {
      status: 'passed',
      summary: passedSummary,
      warnings: allWarnings.length > 0 ? allWarnings : undefined,
      specResults: specResults.length > 0 ? specResults : undefined,
    };
  }

  const summary = verificationResult.summary || '验证失败';
  const details = verificationRunner.getErrorDetails(verificationResult).trim();
  const compactDetails = details.length > 3000 ? `${details.slice(0, 3000)}\n...(错误详情已截断)` : details;
  messageHub.progress('Verification', `❌ 验收失败：${summary}`);
  return {
    status: 'failed',
    summary: `验收失败：${summary}`,
    details: compactDetails || undefined,
    specResults: specResults.length > 0 ? specResults : undefined,
  };
}

/**
 * 对含有 verificationSpec 的 AcceptanceCriterion 进行程序化验证
 */
function runSpecVerifications(
  criteria: AcceptanceCriterion[],
  workspaceRoot: string,
): Array<{ criterionId: string; passed: boolean; detail: string }> {
  const results: Array<{ criterionId: string; passed: boolean; detail: string }> = [];

  for (const criterion of criteria) {
    if (!criterion.verifiable || !criterion.verificationSpec) {
      continue;
    }
    const result = verifySpec(criterion.id, criterion.verificationSpec, workspaceRoot);
    results.push(result);
  }

  return results;
}

/**
 * 执行单个 VerificationSpec 的程序化检查
 */
function verifySpec(
  criterionId: string,
  spec: VerificationSpec,
  workspaceRoot: string,
): { criterionId: string; passed: boolean; detail: string } {
  try {
    switch (spec.type) {
      case 'file_exists': {
        if (!spec.targetPath) {
          return { criterionId, passed: false, detail: 'file_exists: targetPath 未指定' };
        }
        const fullPath = path.resolve(workspaceRoot, spec.targetPath);
        const exists = fs.existsSync(fullPath);
        return {
          criterionId,
          passed: exists,
          detail: exists ? `文件存在: ${spec.targetPath}` : `文件不存在: ${spec.targetPath}`,
        };
      }

      case 'file_content': {
        if (!spec.targetPath || !spec.expectedContent) {
          return { criterionId, passed: false, detail: 'file_content: targetPath 或 expectedContent 未指定' };
        }
        const filePath = path.resolve(workspaceRoot, spec.targetPath);
        if (!fs.existsSync(filePath)) {
          return { criterionId, passed: false, detail: `文件不存在: ${spec.targetPath}` };
        }
        const content = fs.readFileSync(filePath, 'utf-8');
        const mode = spec.contentMatchMode || 'contains';
        let matched = false;
        if (mode === 'exact') {
          matched = content === spec.expectedContent;
        } else if (mode === 'contains') {
          matched = content.includes(spec.expectedContent);
        } else if (mode === 'regex') {
          matched = new RegExp(spec.expectedContent).test(content);
        }
        return {
          criterionId,
          passed: matched,
          detail: matched
            ? `文件内容匹配(${mode}): ${spec.targetPath}`
            : `文件内容不匹配(${mode}): ${spec.targetPath}`,
        };
      }

      case 'test_pass':
      case 'task_completed':
      case 'custom':
        // 外部执行器尚未接入时，不能将该标准视为通过，避免“未验证即通过”。
        return {
          criterionId,
          passed: false,
          detail: `${spec.type}: 需要外部验证器，当前未接入`,
        };

      default:
        return { criterionId, passed: false, detail: `未知验证类型: ${spec.type}` };
    }
  } catch (error: any) {
    return { criterionId, passed: false, detail: `验证异常: ${error.message}` };
  }
}

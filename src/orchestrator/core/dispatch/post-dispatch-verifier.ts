import { logger, LogCategory } from '../../../logging';
import type { MessageHub } from '../message/message-hub';
import { isNonBlockingVerificationWarning, type VerificationRunner } from '../../verification-runner';
import type { DispatchBatch } from './dispatch-batch';
import type {
  AcceptanceBaseVerificationReport,
  AcceptanceCriteriaExecutionSummary,
  AcceptanceCriterion,
  AcceptanceExecutionReport,
  AcceptanceExecutionSkippedReason,
} from '../../mission/types';
import {
  createDefaultValidatorRegistry,
  createProcessVerificationCommandRunner,
  type VerificationCustomValidator,
  type ValidatorRegistry,
} from '../validator-registry';
import {
  buildVerificationId,
  mergeOrchestrationTraceLinks,
} from '../../trace/types';

const defaultValidatorRegistry = createDefaultValidatorRegistry();
const defaultVerificationCommandRunner = createProcessVerificationCommandRunner();

export type DeliveryVerificationOutcome = AcceptanceExecutionReport;

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

function compactDetails(text?: string, limit: number = 3000): string | undefined {
  const normalized = typeof text === 'string' ? text.trim() : '';
  if (!normalized) {
    return undefined;
  }
  return normalized.length > limit ? `${normalized.slice(0, limit)}\n...(错误详情已截断)` : normalized;
}

function buildCriteriaSummary(
  criteriaResults: NonNullable<AcceptanceExecutionReport['criteriaResults']>,
): AcceptanceCriteriaExecutionSummary | undefined {
  if (criteriaResults.length === 0) {
    return undefined;
  }

  const passed = criteriaResults.filter((result) => result.status === 'passed').length;
  return {
    total: criteriaResults.length,
    passed,
    failed: criteriaResults.length - passed,
  };
}

function buildNotRunBaseVerificationReport(summary: string): AcceptanceBaseVerificationReport {
  return {
    status: 'not_run',
    summary,
  };
}

function buildBaseVerificationReport(input: {
  verificationRunner: VerificationRunner;
  verificationResult: Awaited<ReturnType<VerificationRunner['runVerification']>>;
  warnings: string[];
}): AcceptanceBaseVerificationReport {
  const baseSummary = input.verificationResult.success
    ? (input.verificationResult.summary || '基础验收通过')
    : (input.verificationResult.summary || '基础验收失败');
  const baseDetails = input.verificationResult.success
    ? undefined
    : compactDetails(input.verificationRunner.getErrorDetails(input.verificationResult).trim());

  return {
    status: input.verificationResult.success ? 'passed' : 'failed',
    summary: baseSummary,
    details: baseDetails,
    warnings: input.warnings.length > 0 ? input.warnings : undefined,
  };
}

export async function runPostDispatchVerification(
  batch: DispatchBatch,
  verificationRunner: VerificationRunner | undefined,
  messageHub: MessageHub,
  options?: {
    workspaceRoot?: string;
    acceptanceCriteria?: AcceptanceCriterion[];
    validatorRegistry?: ValidatorRegistry;
    customValidators?: Record<string, VerificationCustomValidator>;
  },
): Promise<DeliveryVerificationOutcome> {
  const verificationTrace = mergeOrchestrationTraceLinks(batch.trace, {
    verificationId: buildVerificationId(batch.id),
  });
  const verificationMessageMetadata = verificationTrace
    ? {
      requestId: verificationTrace.requestId,
      missionId: verificationTrace.missionId,
      turnId: verificationTrace.turnId,
      sessionId: verificationTrace.sessionId,
      extra: {
        batchId: verificationTrace.batchId,
        verificationId: verificationTrace.verificationId,
        planId: verificationTrace.planId,
      },
    }
    : undefined;
  if (!verificationRunner) {
    return {
      status: 'skipped',
      summary: '未配置验收执行器',
      skippedReason: 'no_runner',
      baseVerification: buildNotRunBaseVerificationReport('未配置验收执行器'),
      trace: verificationTrace,
    };
  }

  const entries = batch.getEntries();
  if (entries.length === 0) {
    return {
      status: 'skipped',
      summary: '未发现可验收的任务',
      skippedReason: 'no_entries',
      baseVerification: buildNotRunBaseVerificationReport('未发现可验收的任务'),
      trace: verificationTrace,
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
      baseVerification: buildNotRunBaseVerificationReport('子任务失败或取消，跳过验收'),
      trace: verificationTrace,
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
      modifiedFiles,
      baseVerification: buildNotRunBaseVerificationReport('未检测到文件修改，跳过验收'),
      trace: verificationTrace,
    };
  }

  messageHub.progress('Verification', `正在执行验收检查（${modifiedFiles.length} 个修改文件）...`, {
    metadata: verificationMessageMetadata,
  });
  const verificationResult = await verificationRunner.runVerification(batch.id, modifiedFiles);

  // 程序化验证：对含有 verificationSpec 的 AcceptanceCriterion 进行结构化检查
  const criteriaResults = options?.workspaceRoot && options?.acceptanceCriteria
    ? await (options.validatorRegistry || defaultValidatorRegistry).executeCriteria(options.acceptanceCriteria, {
      workspaceRoot: options.workspaceRoot,
      batch,
      modifiedFiles,
      verificationRunner,
      runCommand: defaultVerificationCommandRunner,
      customValidators: options.customValidators,
    })
    : [];
  const criteriaFailures = criteriaResults.filter((result) => result.status === 'failed');
  const criteriaSummary = buildCriteriaSummary(criteriaResults);
  const allWarnings = [...(verificationResult.warnings || [])]
    .filter((warning) => !isNonBlockingVerificationWarning(warning));
  const baseVerification = buildBaseVerificationReport({
    verificationRunner,
    verificationResult,
    warnings: allWarnings,
  });

  if (criteriaFailures.length > 0) {
    const mergedDetails = [
      `基础验收结论：${baseVerification.summary}`,
      baseVerification.details ? `基础验收详情：\n${baseVerification.details}` : '',
      `结构化验收缺口：\n${criteriaFailures.map((result) => `- [${result.criterionId}] ${result.detail}`).join('\n')}`,
    ].filter(item => item.trim().length > 0).join('\n\n');
    const compactMergedDetails = compactDetails(mergedDetails);
    allWarnings.push(...criteriaFailures.map((result) => `[${result.criterionId}] ${result.detail}`));
    messageHub.progress('Verification', `❌ 验收失败：结构化验收未通过（${criteriaFailures.length} 项）`, {
      metadata: verificationMessageMetadata,
    });
    if (allWarnings.length > 0) {
      messageHub.notify(`验收告警：${allWarnings.join('；')}`, 'warning');
    }
    return {
      status: 'failed',
      summary: `验收失败：结构化验收未通过（${criteriaFailures.length} 项）`,
      details: compactMergedDetails,
      warnings: allWarnings.length > 0 ? allWarnings : undefined,
      modifiedFiles,
      baseVerification,
      criteriaResults: criteriaResults.length > 0 ? criteriaResults : undefined,
      criteriaSummary,
      trace: verificationTrace,
    };
  }

  if (verificationResult.success) {
    const passedSummary = verificationResult.summary || '验收通过';
    if (allWarnings.length > 0) {
      messageHub.notify(`验收告警：${allWarnings.join('；')}`, 'warning');
    }
    messageHub.progress('Verification', `✅ ${passedSummary}`, {
      metadata: verificationMessageMetadata,
    });
    return {
      status: 'passed',
      summary: passedSummary,
      warnings: allWarnings.length > 0 ? allWarnings : undefined,
      modifiedFiles,
      baseVerification,
      criteriaResults: criteriaResults.length > 0 ? criteriaResults : undefined,
      criteriaSummary,
      trace: verificationTrace,
    };
  }

  const summary = verificationResult.summary || '验证失败';
  const details = compactDetails(verificationRunner.getErrorDetails(verificationResult).trim());
  messageHub.progress('Verification', `❌ 验收失败：${summary}`, {
    metadata: verificationMessageMetadata,
  });
  return {
    status: 'failed',
    summary: `验收失败：${summary}`,
    details,
    warnings: allWarnings.length > 0 ? allWarnings : undefined,
    modifiedFiles,
    baseVerification,
    criteriaResults: criteriaResults.length > 0 ? criteriaResults : undefined,
    criteriaSummary,
    trace: verificationTrace,
  };
}

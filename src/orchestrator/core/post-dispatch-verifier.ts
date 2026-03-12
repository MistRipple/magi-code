import { logger, LogCategory } from '../../logging';
import type { MessageHub } from './message-hub';
import type { VerificationRunner } from '../verification-runner';
import type { DispatchBatch } from './dispatch-batch';

export type DeliveryVerificationStatus = 'skipped' | 'passed' | 'failed';

export interface DeliveryVerificationOutcome {
  status: DeliveryVerificationStatus;
  summary: string;
  details?: string;
  warnings?: string[];
  skippedReason?: 'no_runner' | 'no_entries' | 'execution_failed' | 'no_changes';
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
  messageHub: MessageHub
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
  if (verificationResult.success) {
    const passedSummary = verificationResult.summary || '验收通过';
    if (verificationResult.warnings && verificationResult.warnings.length > 0) {
      messageHub.notify(`验收告警：${verificationResult.warnings.join('；')}`, 'warning');
    }
    messageHub.progress('Verification', `✅ ${passedSummary}`);
    return {
      status: 'passed',
      summary: passedSummary,
      warnings: verificationResult.warnings,
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
  };
}

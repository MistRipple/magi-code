#!/usr/bin/env node
/**
 * Context 治理回归
 *
 * 覆盖目标：
 * 1) auto compact 达阈值后自动执行并写入 archival 记录
 * 2) manual compact（context_compact）可显式触发并写入 archival 记录
 */

const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

async function main() {
  const contextManagerOut = path.join(OUT, 'context', 'context-manager.js');
  const contextTypesOut = path.join(OUT, 'context', 'types.js');
  const orchestrationOut = path.join(OUT, 'tools', 'orchestration-executor.js');

  if (!fs.existsSync(contextManagerOut) || !fs.existsSync(contextTypesOut) || !fs.existsSync(orchestrationOut)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { ContextManager } = require(contextManagerOut);
  const { DEFAULT_CONTEXT_CONFIG } = require(contextTypesOut);
  const { OrchestrationExecutor } = require(orchestrationOut);

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-context-governance-'));

  try {
    const config = JSON.parse(JSON.stringify(DEFAULT_CONTEXT_CONFIG));
    config.compression.tokenLimit = 120;
    config.compression.lineLimit = 20;

    const contextManager = new ContextManager(workspaceRoot, config);
    const sessionId = 'session-context-governance';
    await contextManager.initialize(sessionId, 'context-governance');

    contextManager.setPrimaryIntent('验证三层上下文治理闭环');
    contextManager.addUserConstraint('必须保留关键任务目标');
    contextManager.addTask({
      id: 'task-context-1',
      description: '压缩治理回归',
      status: 'in_progress',
      assignedWorker: 'orchestrator',
    });
    contextManager.setCurrentWork('正在构造超长上下文触发 auto compact');
    contextManager.addDecision('decision-context-1', '启用上下文归档', '为长会话提供可追踪压缩事件');
    contextManager.addImportantContext('A'.repeat(6000));
    contextManager.addToolOutput('worker_wait', JSON.stringify({
      wait_status: 'completed',
      results: [{ task_id: 'task-context-1', worker: 'codex', status: 'completed', summary: 'ok' }],
    }));

    await contextManager.flushMemorySave();

    const archiveFile = path.join(workspaceRoot, config.storagePath, sessionId, 'memory-archival.jsonl');
    assert(fs.existsSync(archiveFile), '未生成上下文压缩归档文件');

    let archiveLines = fs.readFileSync(archiveFile, 'utf8').trim().split('\n').filter(Boolean);
    const hasAutoRecord = archiveLines.some((line) => line.includes('"reason":"auto"'));
    assert(hasAutoRecord, '缺少 auto compact 归档记录');

    const orchestrationExecutor = new OrchestrationExecutor();
    orchestrationExecutor.setHandlers({
      compactContext: async (params) => contextManager.manualCompactMemory({
        force: params.force === true,
        note: typeof params.reason === 'string' ? params.reason : undefined,
      }),
    });

    const manualToolResult = await orchestrationExecutor.execute({
      id: 'tool-call-context-compact',
      name: 'context_compact',
      arguments: {
        force: true,
        reason: 'manual-regression',
      },
    });
    assert(!manualToolResult.isError, 'context_compact 工具执行失败');
    const manualPayload = JSON.parse(manualToolResult.content);
    assert(manualPayload.success === true, 'manual compact 未返回 success=true');
    assert(manualPayload.archived === true, 'manual compact 未写入归档');

    await contextManager.flushMemorySave();

    archiveLines = fs.readFileSync(archiveFile, 'utf8').trim().split('\n').filter(Boolean);
    const hasManualRecord = archiveLines.some((line) => line.includes('"reason":"manual"'));
    const hasManualNote = archiveLines.some((line) => line.includes('"note":"manual-regression"'));
    assert(hasManualRecord, '缺少 manual compact 归档记录');
    assert(hasManualNote, '缺少 manual compact 归档备注');

    console.log('\n=== context governance regression ===');
    console.log(JSON.stringify({
      sessionId,
      archiveFile,
      archiveRecords: archiveLines.length,
      checks: {
        autoCompactionArchived: hasAutoRecord,
        manualCompactionArchived: hasManualRecord,
        manualCompactionNotePersisted: hasManualNote,
      },
      pass: true,
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('context governance 回归失败:', error?.stack || error);
  process.exit(1);
});


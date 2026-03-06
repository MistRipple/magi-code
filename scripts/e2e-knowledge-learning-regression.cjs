#!/usr/bin/env node
/**
 * 知识库 Learning 链路回归
 *
 * 覆盖：
 * 1) Learning 质量过滤 + 去重
 * 2) 会话 Learning 提取（无 LLM 启发式兜底）
 * 3) WisdomManager -> ProjectKnowledgeBase 的存储链路
 */

const fs = require('fs');
const fsp = require('fs/promises');
const os = require('os');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

async function writeFile(absPath, content) {
  await fsp.mkdir(path.dirname(absPath), { recursive: true });
  await fsp.writeFile(absPath, content, 'utf-8');
}

async function main() {
  const kbModule = path.join(OUT, 'knowledge', 'project-knowledge-base.js');
  const wisdomModule = path.join(OUT, 'orchestrator', 'wisdom', 'wisdom-extractor.js');
  const helperModule = path.join(OUT, 'orchestrator', 'core', 'mission-driven-engine-helpers.js');
  if (!fs.existsSync(kbModule) || !fs.existsSync(wisdomModule) || !fs.existsSync(helperModule)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { ProjectKnowledgeBase } = require(kbModule);
  const { WisdomManager } = require(wisdomModule);
  const { createWisdomStorage } = require(helperModule);

  const tempRoot = await fsp.mkdtemp(path.join(os.tmpdir(), 'magi-learning-regression-'));
  try {
    await writeFile(path.join(tempRoot, 'package.json'), JSON.stringify({
      name: 'magi-learning-regression',
      version: '1.0.0',
    }, null, 2));
    await writeFile(path.join(tempRoot, 'src', 'index.ts'), 'export const ok = true;\n');

    const kb = new ProjectKnowledgeBase({
      projectRoot: tempRoot,
      storageDir: path.join(tempRoot, '.magi', 'knowledge'),
    });
    await kb.initialize();

    const learningBefore = kb.getLearnings().length;

    const invalid = kb.addLearning('待处理', 'session:test');
    assert(invalid.status === 'rejected', '质量过滤失败：低价值 learning 未被过滤');

    const primary = kb.addLearning(
      '部署前必须先执行数据库迁移，避免启动后 schema 不一致。',
      'worker:claude'
    );
    assert(primary.status === 'inserted' && primary.record && primary.record.id, '添加有效 learning 失败');

    const same = kb.addLearning(
      '部署前必须先执行数据库迁移，避免启动后 schema 不一致。',
      'worker:claude'
    );
    assert(
      same.status === 'duplicate'
      && same.record
      && primary.record
      && same.record.id === primary.record.id,
      '重复 learning 未命中去重'
    );

    const near = kb.addLearning(
      '部署前必须先执行数据库迁移，避免启动后schema不一致',
      'worker:claude'
    );
    assert(
      near.status === 'duplicate'
      && near.record
      && primary.record
      && near.record.id === primary.record.id,
      '近似 learning 未命中去重'
    );

    const learningAfterDedup = kb.getLearnings().length;
    assert(
      learningAfterDedup === learningBefore + 1,
      `去重后 learning 数量异常，期望 ${learningBefore + 1}，实际 ${learningAfterDedup}`
    );

    const heuristicCandidates = await kb.extractLearningsFromSession([
      { role: 'user', content: '这次发布流程为什么失败？' },
      { role: 'assistant', content: '经验：先运行 migration 再启动服务。注意：并行改动前先 file_view 获取最新锚点。' },
    ]);
    assert(heuristicCandidates.length >= 1, '启发式 learning 提取失败：未提取任何候选');

    const hasExpectedHeuristic = heuristicCandidates.some((candidate) =>
      candidate.content.includes('migration') || candidate.content.includes('file_view')
    );
    assert(hasExpectedHeuristic, '启发式 learning 提取失败：未命中预期经验内容');

    const fakeContextManager = {
      addImportantContext() {},
      addDecision() {},
      addPendingIssue() {},
    };
    const storage = createWisdomStorage(fakeContextManager, () => kb);
    const wisdomManager = new WisdomManager(storage);

    const wisdomResult = wisdomManager.processReport({
      type: 'completed',
      workerId: 'claude',
      assignmentId: 'assignment-learning',
      timestamp: Date.now(),
      result: {
        success: true,
        modifiedFiles: [],
        createdFiles: [],
        summary: '重要：先运行 migration 再启动服务。发现：并行编辑前先 file_view 获取最新锚点。',
        totalDuration: 1200,
      },
    }, 'assignment-learning');

    assert(
      wisdomResult.significantLearning || wisdomResult.learnings.length > 0,
      'Wisdom 提取失败：未产出可用 learning'
    );

    const finalLearnings = kb.getLearnings();
    const hasWisdomLearning = finalLearnings.some((record) =>
      record.content.includes('migration') || record.content.includes('file_view')
    );
    assert(hasWisdomLearning, 'Wisdom 存储失败：未写入知识库 learnings');

    const summary = {
      pass: true,
      checks: {
        qualityGate: invalid.status === 'rejected',
        dedupStable: learningAfterDedup === learningBefore + 1,
        heuristicExtraction: hasExpectedHeuristic,
        wisdomStorage: hasWisdomLearning,
      },
      counts: {
        before: learningBefore,
        afterDedup: learningAfterDedup,
        final: finalLearnings.length,
      },
    };

    console.log('\n=== 知识库 Learning 回归结果 ===');
    console.log(JSON.stringify(summary, null, 2));
    process.exit(0);
  } finally {
    await fsp.rm(tempRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('知识库 Learning 回归失败:', error?.stack || error);
  process.exit(1);
});

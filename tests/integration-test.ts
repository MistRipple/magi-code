/**
 * 任务系统和快照系统集成测试
 */

import * as path from 'path';
import * as fs from 'fs';

const TEST_WORKSPACE = path.join(__dirname, '../.test-workspace');

function ensureDir(dir: string): void {
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
}

function cleanup(): void {
  if (fs.existsSync(TEST_WORKSPACE)) fs.rmSync(TEST_WORKSPACE, { recursive: true, force: true });
}

async function testSnapshotManager(): Promise<boolean> {
  console.log('\n=== 测试 SnapshotManager ===');
  const { SnapshotManager } = require('../out/snapshot-manager');
  const { SessionManager } = require('../out/session-manager');
  
  ensureDir(TEST_WORKSPACE);
  const testFile = path.join(TEST_WORKSPACE, 'test-file.ts');
  const originalContent = 'const x = 1;\n';
  fs.writeFileSync(testFile, originalContent);
  
  const sessionManager = new SessionManager(TEST_WORKSPACE);
  const snapshotManager = new SnapshotManager(sessionManager, TEST_WORKSPACE);
  sessionManager.createSession();
  
  console.log('  测试1: 创建快照...');
  const snapshot = snapshotManager.createSnapshot('test-file.ts', 'claude', 'subtask-1');
  if (!snapshot) { console.log('  ❌ 创建快照失败'); return false; }
  console.log('  ✅ 快照创建成功');
  
  console.log('  测试2: 修改文件...');
  fs.writeFileSync(testFile, 'const x = 2;\nconst y = 3;\n');
  
  console.log('  测试3: 获取待处理变更...');
  const changes = snapshotManager.getPendingChanges();
  if (changes.length === 0) { console.log('  ❌ 未检测到变更'); return false; }
  console.log('  ✅ 检测到 ' + changes.length + ' 个变更');
  
  console.log('  测试4: 还原快照...');
  const reverted = snapshotManager.revertToSnapshot('test-file.ts');
  if (!reverted) { console.log('  ❌ 还原失败'); return false; }
  const revertedContent = fs.readFileSync(testFile, 'utf-8');
  if (revertedContent !== originalContent) { console.log('  ❌ 还原内容不匹配'); return false; }
  console.log('  ✅ 还原成功');
  return true;
}

async function testTaskManager(): Promise<boolean> {
  console.log('\n=== 测试 TaskManager ===');
  const { TaskManager } = require('../out/task-manager');
  const { SessionManager } = require('../out/session-manager');

  ensureDir(TEST_WORKSPACE);
  const sessionManager = new SessionManager(TEST_WORKSPACE);
  const taskManager = new TaskManager(sessionManager);
  sessionManager.createSession();

  console.log('  测试1: 创建任务...');
  const task = taskManager.createTask('测试任务');
  if (!task?.id) { console.log('  ❌ 创建任务失败'); return false; }
  console.log('  ✅ 任务创建成功: ' + task.id);

  console.log('  测试2: 添加子任务（使用新的统一类型）...');
  // 新的 addSubTask 签名: (taskId, description, assignedWorker, targetFiles, options?)
  const subTask = taskManager.addSubTask(task.id, '子任务描述', 'claude', ['test.ts'], {
    reason: '测试原因',
    prompt: '执行测试任务',
  });
  if (!subTask?.id) { console.log('  ❌ 添加子任务失败'); return false; }
  if (subTask.assignedWorker !== 'claude') { console.log('  ❌ assignedWorker 不正确'); return false; }
  console.log('  ✅ 子任务添加成功: ' + subTask.id);

  console.log('  测试3: 更新任务状态...');
  taskManager.updateTaskStatus(task.id, 'running');
  const updatedTask = taskManager.getTask(task.id);
  if (updatedTask?.status !== 'running') { console.log('  ❌ 任务状态更新失败'); return false; }
  console.log('  ✅ 任务状态更新成功');
  return true;
}

async function runAllTests(): Promise<void> {
  console.log('========================================');
  console.log('  任务系统和快照系统集成测试');
  console.log('========================================');

  cleanup(); ensureDir(TEST_WORKSPACE);
  const results: { name: string; passed: boolean }[] = [];

  try {
    results.push({ name: 'SnapshotManager', passed: await testSnapshotManager() });
    results.push({ name: 'TaskManager', passed: await testTaskManager() });
  } catch (error) {
    console.error('\n❌ 测试执行出错:', error);
  } finally { cleanup(); }
  
  console.log('\n========================================');
  console.log('  测试结果汇总');
  console.log('========================================');
  
  let passedCount = 0, failedCount = 0;
  for (const r of results) {
    console.log('  ' + (r.passed ? '✅ PASS' : '❌ FAIL') + ': ' + r.name);
    r.passed ? passedCount++ : failedCount++;
  }
  console.log('----------------------------------------');
  console.log('  总计: ' + results.length + ' | 通过: ' + passedCount + ' | 失败: ' + failedCount);
  console.log('========================================\n');
  if (failedCount > 0) process.exit(1);
}

runAllTests().catch(console.error);


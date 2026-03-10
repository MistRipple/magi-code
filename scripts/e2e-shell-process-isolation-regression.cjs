#!/usr/bin/env node
/**
 * Shell 进程隔离回归脚本
 *
 * 目标：
 * 1) 相同 agent 连续两次 launch-process(task) 必须分配不同终端会话
 * 2) 两个进程输出互不污染
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

async function sleep(ms) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitUntilTerminal(executor, terminalId, timeoutMs = 20000) {
  const start = Date.now();
  let cursor = 0;
  let mergedOutput = '';
  let finalStatus = 'running';
  while (Date.now() - start < timeoutMs) {
    const read = await executor.readProcess(terminalId, false, 1, cursor);
    if (typeof read.output === 'string' && read.output.length > 0) {
      mergedOutput += read.output;
    }
    if (Number.isInteger(read.next_cursor)) {
      cursor = read.next_cursor;
    } else if (Number.isInteger(read.output_cursor)) {
      cursor = read.output_cursor;
    }
    finalStatus = read.status;
    if (finalStatus === 'completed' || finalStatus === 'failed' || finalStatus === 'killed' || finalStatus === 'timeout') {
      return { status: finalStatus, output: mergedOutput };
    }
    await sleep(80);
  }
  throw new Error(`等待进程结束超时: terminal_id=${terminalId}`);
}

async function main() {
  const executorPath = path.join(OUT, 'tools', 'shell', 'node-shell-executor.js');
  if (!fs.existsSync(executorPath)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { NodeShellExecutor } = require(executorPath);
  const executor = new NodeShellExecutor();

  try {
    const launch1 = await executor.launchProcess({
      name: 'orchestrator',
      command: 'echo MAGI_ISO_FIRST',
      runMode: 'task',
      wait: false,
      maxWaitSeconds: 10,
      cwd: ROOT,
    });

    const first = await waitUntilTerminal(executor, launch1.terminal_id);

    const launch2 = await executor.launchProcess({
      name: 'orchestrator',
      command: 'echo MAGI_ISO_SECOND',
      runMode: 'task',
      wait: false,
      maxWaitSeconds: 10,
      cwd: ROOT,
    });

    const second = await waitUntilTerminal(executor, launch2.terminal_id);

    assert(first.status === 'completed', `first 进程状态异常: ${first.status}`);
    assert(second.status === 'completed', `second 进程状态异常: ${second.status}`);
    assert(launch1.terminal_id !== launch2.terminal_id, '连续两次 launch-process 得到了相同 terminal_id');
    assert(
      typeof launch1.terminal_name === 'string'
      && typeof launch2.terminal_name === 'string'
      && launch1.terminal_name !== launch2.terminal_name,
      `连续两次 launch-process 终端名称未隔离: ${launch1.terminal_name} vs ${launch2.terminal_name}`,
    );
    assert(first.output.includes('MAGI_ISO_FIRST'), 'first 输出缺少 MAGI_ISO_FIRST');
    assert(second.output.includes('MAGI_ISO_SECOND'), 'second 输出缺少 MAGI_ISO_SECOND');
    assert(!first.output.includes('MAGI_ISO_SECOND'), 'first 输出出现 second 内容，存在串流');
    assert(!second.output.includes('MAGI_ISO_FIRST'), 'second 输出出现 first 内容，存在串流');

    // task 进程完成后应释放底层 shell 会话，避免会话泄漏
    const managedSessions = executor.managedSessions || executor['managedSessions'];
    if (managedSessions && typeof managedSessions.size === 'number') {
      assert(
        managedSessions.size === 0,
        `task 完成后 managedSessions 未清空，存在会话泄漏: size=${managedSessions.size}`,
      );
    }

    console.log('\n=== shell process isolation regression ===');
    console.log(JSON.stringify({
      pass: true,
      launches: [
        { terminalId: launch1.terminal_id, terminalName: launch1.terminal_name, status: first.status },
        { terminalId: launch2.terminal_id, terminalName: launch2.terminal_name, status: second.status },
      ],
      checks: [
        'isolated-terminal-name-per-launch',
        'isolated-output-per-process',
        'task-session-cleanup',
      ],
    }, null, 2));
    process.exit(0);
  } finally {
    executor.dispose();
  }
}

main().catch((error) => {
  console.error('shell process isolation 回归失败:', error?.stack || error);
  process.exit(1);
});

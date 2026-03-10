#!/usr/bin/env node
/**
 * Shell task 基本能力回归脚本
 *
 * 目标：
 * 1. 验证 launch-process(run_mode=task, wait=true) 能执行并捕获输出
 * 2. 验证 read-process 能读取已完成的 task 输出
 * 3. 验证 kill-process 可终止运行中的 task 子进程
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

async function main() {
  const executorPath = path.join(OUT, 'tools', 'shell', 'node-shell-executor.js');
  if (!fs.existsSync(executorPath)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { NodeShellExecutor } = require(executorPath);
  const executor = new NodeShellExecutor();

  try {
    // 场景 1：快速 task 执行并等待完成
    const fastTask = await executor.launchProcess({
      name: 'orchestrator',
      command: 'echo MAGI_SHELL_TASK_OK',
      runMode: 'task',
      wait: true,
      maxWaitSeconds: 10,
      cwd: ROOT,
    });

    assert(fastTask.status === 'completed',
      `task 应 completed，实际=${fastTask.status}, output=${fastTask.output}`);
    assert(
      typeof fastTask.output === 'string' && fastTask.output.includes('MAGI_SHELL_TASK_OK'),
      `task 输出缺失，output=${fastTask.output}`
    );

    // 场景 2：读取已完成 task 的输出
    const readFastTask = await executor.readProcess(fastTask.terminal_id, false, 1);
    assert(readFastTask.status === 'completed',
      `read-process 应返回 completed，实际=${readFastTask.status}`);

    // 场景 3：启动长时间 task 并 kill
    const longTask = await executor.launchProcess({
      name: 'orchestrator',
      command: 'sleep 30',
      runMode: 'task',
      wait: false,
      maxWaitSeconds: 1,
      cwd: ROOT,
    });
    assert(longTask.status === 'running' || longTask.status === 'starting',
      `sleep task 初始状态异常: ${longTask.status}`);

    await new Promise((resolve) => setTimeout(resolve, 200));
    const killed = await executor.killProcess(longTask.terminal_id);
    assert(killed.killed === true, 'kill-process 未返回 killed=true');

    const readKilled = await executor.readProcess(longTask.terminal_id, false, 1);
    assert(readKilled.status === 'killed' || readKilled.status === 'failed',
      `被 kill 的 task 状态异常: ${readKilled.status}`);

    console.log('\n=== shell task 基本能力回归结果 ===');
    console.log(JSON.stringify({
      pass: true,
      fastTask: {
        terminal_id: fastTask.terminal_id,
        status: fastTask.status,
        return_code: fastTask.return_code,
      },
      longTask: {
        terminal_id: longTask.terminal_id,
        kill_return_code: killed.return_code,
        final_status: readKilled.status,
      },
    }, null, 2));
    process.exit(0);
  } finally {
    executor.dispose();
  }
}

main().catch((error) => {
  console.error('shell task 基本能力回归失败:', error?.stack || error);
  process.exit(1);
});

#!/usr/bin/env node
/**
 * Shell task fallback 回归脚本
 *
 * 目标：
 * 1. 强制模拟 ScriptCapture 不可用（script/pgrep 缺失场景）
 * 2. 验证 launch-process(run_mode=task) 仍能执行并捕获输出
 * 3. 验证 kill-process 可终止直连 task 子进程，且内部映射能清理
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
    const strategy = executor.scriptCaptureStrategy || executor['scriptCaptureStrategy'];
    assert(strategy, '无法访问 ScriptCaptureStrategy 实例');

    // 强制模拟 script/pgrep 不可用：setup 失败 + 永远 not ready
    strategy.setupTerminal = async () => false;
    strategy.ensureTerminalSessionActive = async () => false;
    strategy.isReady = () => false;

    const fastTask = await executor.launchProcess({
      name: 'orchestrator',
      command: 'echo MAGI_SHELL_TASK_FALLBACK_OK',
      runMode: 'task',
      wait: true,
      maxWaitSeconds: 10,
      cwd: ROOT,
    });

    assert(fastTask.status === 'completed',
      `fallback task 应 completed，实际=${fastTask.status}, output=${fastTask.output}`);
    assert(
      typeof fastTask.output === 'string' && fastTask.output.includes('MAGI_SHELL_TASK_FALLBACK_OK'),
      `fallback task 输出缺失，output=${fastTask.output}`
    );

    const readFastTask = await executor.readProcess(fastTask.terminal_id, false, 1);
    assert(readFastTask.status === 'completed',
      `read-process 应返回 completed，实际=${readFastTask.status}`);

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

    const directTaskMap = executor.directTaskProcesses || executor['directTaskProcesses'];
    assert(directTaskMap && directTaskMap.size === 0, 'directTaskProcesses 未清理干净');

    console.log('\n=== shell task fallback 回归结果 ===');
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
  console.error('shell task fallback 回归失败:', error?.stack || error);
  process.exit(1);
});

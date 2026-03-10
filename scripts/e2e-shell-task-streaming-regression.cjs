#!/usr/bin/env node
/**
 * Shell task 流式增量回归脚本
 *
 * 目标：
 * 1) 验证 launch-process(run_mode=task) 快速返回 running/starting
 * 2) 验证 read-process(from_cursor) 在任务执行期间能拿到多次 delta 增量
 * 3) 验证 task 模式不应出现 startup handshake 超时语义（startup_status/message）
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

async function main() {
  const executorPath = path.join(OUT, 'tools', 'shell', 'node-shell-executor.js');
  if (!fs.existsSync(executorPath)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { NodeShellExecutor } = require(executorPath);
  const executor = new NodeShellExecutor();

  async function runScenario(label, readWait) {
    // 约 3 秒持续输出，确保可观测到多轮增量
    const command = "for i in 1 2 3 4 5 6; do echo MAGI_STREAM_$i; sleep 0.5; done";

    const launch = await executor.launchProcess({
      name: 'orchestrator',
      command,
      runMode: 'task',
      wait: false,
      maxWaitSeconds: 10,
      cwd: ROOT,
    });

    assert(
      launch.status === 'running' || launch.status === 'starting' || launch.status === 'queued',
      `[${label}] task launch 应快返为运行态，实际=${launch.status}`,
    );

    const chunks = [];
    let cursor = Number.isInteger(launch.output_cursor) ? launch.output_cursor : 0;
    let finalStatus = launch.status;
    let seenRunningRound = false;

    for (let round = 0; round < 40; round += 1) {
      const read = await executor.readProcess(launch.terminal_id, readWait, 2, cursor);
      assert(
        read.terminal_id === launch.terminal_id,
        `[${label}] read-process terminal_id 不匹配，预期=${launch.terminal_id} 实际=${read.terminal_id}`,
      );

      // task 模式不应携带 service 启动握手语义
      assert(
        read.run_mode === 'task',
        `[${label}] read-process run_mode 预期 task，实际=${read.run_mode}`,
      );

      if (typeof read.startup_status !== 'undefined' || typeof read.startup_message !== 'undefined') {
        throw new Error(
          `[${label}] task read-process 不应包含 startup 字段, startup_status=${read.startup_status}, startup_message=${read.startup_message}`,
        );
      }

      if (typeof read.output === 'string' && read.output.length > 0) {
        chunks.push({
          from: read.from_cursor,
          to: read.next_cursor,
          output: read.output,
          delta: read.delta,
        });
      }

      if (read.status === 'running' || read.status === 'starting') {
        seenRunningRound = true;
      }

      if (Number.isInteger(read.next_cursor)) {
        cursor = read.next_cursor;
      } else if (Number.isInteger(read.output_cursor)) {
        cursor = read.output_cursor;
      }

      finalStatus = read.status;
      if (finalStatus === 'completed' || finalStatus === 'failed' || finalStatus === 'killed' || finalStatus === 'timeout') {
        break;
      }

      if (!readWait) {
        await sleep(120);
      }
    }

    assert(finalStatus === 'completed', `[${label}] task 最终状态应 completed，实际=${finalStatus}`);

    const allOutput = chunks.map((chunk) => chunk.output).join('');
    for (let i = 1; i <= 6; i += 1) {
      assert(allOutput.includes(`MAGI_STREAM_${i}`), `[${label}] 缺少输出分片 MAGI_STREAM_${i}`);
    }

    // 至少两次增量，才算“执行期间有动态流式更新”
    assert(chunks.length >= 2, `[${label}] 增量轮次不足，预期>=2，实际=${chunks.length}`);
    // wait=true 语义必须可在任务结束前返回 running 轮次，避免“结束后一次性渲染”
    if (readWait) {
      assert(seenRunningRound, `[${label}] wait=true 未观测到运行中轮次，可能仍存在阻塞到完成的问题`);
    }

    return {
      label,
      readWait,
      terminalId: launch.terminal_id,
      chunks: chunks.length,
      finalStatus,
      seenRunningRound,
      sample: chunks.slice(0, 3),
    };
  }

  try {
    const nonBlocking = await runScenario('non-blocking-read', false);
    const blocking = await runScenario('blocking-read', true);

    console.log('\n=== shell task streaming 回归结果 ===');
    console.log(JSON.stringify({
      pass: true,
      scenarios: [nonBlocking, blocking],
    }, null, 2));
    process.exit(0);
  } finally {
    executor.dispose();
  }
}

main().catch((error) => {
  console.error('shell task streaming 回归失败:', error?.stack || error);
  process.exit(1);
});

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

  try {
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
      `task launch 应快返为运行态，实际=${launch.status}`,
    );

    const chunks = [];
    let cursor = Number.isInteger(launch.output_cursor) ? launch.output_cursor : 0;
    let finalStatus = launch.status;

    for (let round = 0; round < 40; round += 1) {
      const read = await executor.readProcess(launch.terminal_id, false, 1, cursor);

      // task 模式不应携带 service 启动握手语义
      assert(
        read.run_mode === 'task',
        `read-process run_mode 预期 task，实际=${read.run_mode}`,
      );

      if (typeof read.startup_status !== 'undefined' || typeof read.startup_message !== 'undefined') {
        throw new Error(
          `task read-process 不应包含 startup 字段, startup_status=${read.startup_status}, startup_message=${read.startup_message}`,
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

      if (Number.isInteger(read.next_cursor)) {
        cursor = read.next_cursor;
      } else if (Number.isInteger(read.output_cursor)) {
        cursor = read.output_cursor;
      }

      finalStatus = read.status;
      if (finalStatus === 'completed' || finalStatus === 'failed' || finalStatus === 'killed' || finalStatus === 'timeout') {
        break;
      }

      await sleep(120);
    }

    assert(finalStatus === 'completed', `task 最终状态应 completed，实际=${finalStatus}`);

    const allOutput = chunks.map((chunk) => chunk.output).join('');
    for (let i = 1; i <= 6; i += 1) {
      assert(allOutput.includes(`MAGI_STREAM_${i}`), `缺少输出分片 MAGI_STREAM_${i}`);
    }

    // 至少两次增量，才算“执行期间有动态流式更新”
    assert(chunks.length >= 2, `增量轮次不足，预期>=2，实际=${chunks.length}`);

    console.log('\n=== shell task streaming 回归结果 ===');
    console.log(JSON.stringify({
      pass: true,
      terminal_id: launch.terminal_id,
      chunks: chunks.length,
      finalStatus,
      sample: chunks.slice(0, 3),
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

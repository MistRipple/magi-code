#!/usr/bin/env node
/**
 * Shell ownership gate 回归脚本
 *
 * 目标：
 * 1) orchestrator 与 worker 启动的 terminal_id 必须相互隔离
 * 2) 跨主体 read-process/kill-process 必须被拒绝
 * 3) 同主体 read-process/kill-process 必须可执行
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function installVscodeStub() {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {},
        window: {},
        Uri: { file: (p) => ({ fsPath: p }) },
        Position: class Position { constructor(line, character) { this.line = line; this.character = character; } },
        Range: class Range { constructor(start, end) { this.start = start; this.end = end; } },
        Selection: class Selection { constructor(anchor, active) { this.anchor = anchor; this.active = active; } },
        RelativePattern: class RelativePattern { constructor(base, pattern) { this.baseUri = base; this.pattern = pattern; } },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

function parseToolJson(result) {
  try {
    return JSON.parse(result.content);
  } catch {
    return null;
  }
}

async function main() {
  const managerPath = path.join(OUT, 'tools', 'tool-manager.js');
  if (!fs.existsSync(managerPath)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  installVscodeStub();
  const { ToolManager } = require(managerPath);
  const toolManager = new ToolManager({
    workspaceRoot: ROOT,
    permissions: { allowEdit: true, allowBash: true, allowWeb: true },
  });

  const launched = [];
  try {
    const orchLaunchRaw = await toolManager.executeInternalTool({
      id: 'own-launch-orchestrator',
      name: 'launch-process',
      arguments: {
        command: 'sleep 30',
        run_mode: 'task',
        wait: false,
        cwd: ROOT,
      },
    }, undefined, { workerId: 'orchestrator', role: 'orchestrator' });

    const orchLaunch = parseToolJson(orchLaunchRaw);
    assert(orchLaunch && Number.isInteger(orchLaunch.terminal_id), 'orchestrator launch-process 失败');
    launched.push({ terminalId: orchLaunch.terminal_id, workerId: 'orchestrator' });

    const workerLaunchRaw = await toolManager.executeInternalTool({
      id: 'own-launch-worker',
      name: 'launch-process',
      arguments: {
        command: 'sleep 30',
        run_mode: 'task',
        wait: false,
        cwd: ROOT,
      },
    }, undefined, { workerId: 'claude', role: 'worker' });

    const workerLaunch = parseToolJson(workerLaunchRaw);
    assert(workerLaunch && Number.isInteger(workerLaunch.terminal_id), 'worker launch-process 失败');
    launched.push({ terminalId: workerLaunch.terminal_id, workerId: 'claude' });
    assert(
      orchLaunch.terminal_id !== workerLaunch.terminal_id,
      '不同主体 launch-process 得到了相同 terminal_id',
    );

    const crossReadByOrchestrator = await toolManager.executeInternalTool({
      id: 'cross-read-by-orchestrator',
      name: 'read-process',
      arguments: { terminal_id: workerLaunch.terminal_id, wait: false, from_cursor: 0 },
    }, undefined, { workerId: 'orchestrator', role: 'orchestrator' });
    assert(crossReadByOrchestrator.isError === true, 'orchestrator 跨主体 read-process 未被拒绝');

    const crossReadByWorker = await toolManager.executeInternalTool({
      id: 'cross-read-by-worker',
      name: 'read-process',
      arguments: { terminal_id: orchLaunch.terminal_id, wait: false, from_cursor: 0 },
    }, undefined, { workerId: 'claude', role: 'worker' });
    assert(crossReadByWorker.isError === true, 'worker 跨主体 read-process 未被拒绝');

    const ownReadByOrchestrator = await toolManager.executeInternalTool({
      id: 'own-read-orchestrator',
      name: 'read-process',
      arguments: { terminal_id: orchLaunch.terminal_id, wait: false, from_cursor: 0 },
    }, undefined, { workerId: 'orchestrator', role: 'orchestrator' });
    assert(ownReadByOrchestrator.isError === false, 'orchestrator 自有 read-process 被错误拒绝');

    const ownReadByWorker = await toolManager.executeInternalTool({
      id: 'own-read-worker',
      name: 'read-process',
      arguments: { terminal_id: workerLaunch.terminal_id, wait: false, from_cursor: 0 },
    }, undefined, { workerId: 'claude', role: 'worker' });
    assert(ownReadByWorker.isError === false, 'worker 自有 read-process 被错误拒绝');

    const crossKillByOrchestrator = await toolManager.executeInternalTool({
      id: 'cross-kill-by-orchestrator',
      name: 'kill-process',
      arguments: { terminal_id: workerLaunch.terminal_id },
    }, undefined, { workerId: 'orchestrator', role: 'orchestrator' });
    assert(crossKillByOrchestrator.isError === true, 'orchestrator 跨主体 kill-process 未被拒绝');

    const ownKillByOrchestrator = await toolManager.executeInternalTool({
      id: 'own-kill-orchestrator',
      name: 'kill-process',
      arguments: { terminal_id: orchLaunch.terminal_id },
    }, undefined, { workerId: 'orchestrator', role: 'orchestrator' });
    assert(ownKillByOrchestrator.isError === false, 'orchestrator 自有 kill-process 失败');

    const ownKillByWorker = await toolManager.executeInternalTool({
      id: 'own-kill-worker',
      name: 'kill-process',
      arguments: { terminal_id: workerLaunch.terminal_id },
    }, undefined, { workerId: 'claude', role: 'worker' });
    assert(ownKillByWorker.isError === false, 'worker 自有 kill-process 失败');

    console.log('\n=== shell ownership gate regression ===');
    console.log(JSON.stringify({
      pass: true,
      terminals: {
        orchestrator: orchLaunch.terminal_id,
        worker: workerLaunch.terminal_id,
      },
      checks: [
        'isolated-terminal-id-by-owner',
        'cross-owner-read-rejected',
        'cross-owner-kill-rejected',
        'owner-read-allowed',
        'owner-kill-allowed',
      ],
    }, null, 2));
    process.exit(0);
  } finally {
    for (const item of launched) {
      try {
        await toolManager.executeInternalTool({
          id: `cleanup-kill-${item.workerId}-${item.terminalId}`,
          name: 'kill-process',
          arguments: { terminal_id: item.terminalId },
        }, undefined, { workerId: item.workerId, role: item.workerId === 'orchestrator' ? 'orchestrator' : 'worker' });
      } catch {
        // 忽略清理失败，避免覆盖主断言
      }
    }
    try {
      toolManager.terminalExecutor?.dispose?.();
    } catch {
      // ignore
    }
  }
}

main().catch((error) => {
  console.error('shell ownership gate 回归失败:', error?.stack || error);
  process.exit(1);
});

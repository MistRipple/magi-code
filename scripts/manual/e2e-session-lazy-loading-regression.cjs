#!/usr/bin/env node
/**
 * Session 懒加载回归脚本
 *
 * 目标：
 * 1) 启动时仅加载会话元数据（不全量加载会话体）
 * 2) getSessionMetas 返回完整元数据列表
 * 3) getSession 可按需加载未预载的会话
 *
 * 运行：
 *   npm run -s compile
 *   node scripts/manual/e2e-session-lazy-loading-regression.cjs
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

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function writeSession(baseDir, id, ts) {
  const sessionDir = path.join(baseDir, id);
  fs.mkdirSync(sessionDir, { recursive: true });
  const session = {
    id,
    status: 'active',
    createdAt: ts,
    updatedAt: ts,
    messages: [
      {
        id: `msg-${id}`,
        role: 'user',
        content: `hello-${id}`,
        timestamp: ts,
      },
    ],
    snapshots: [],
  };
  fs.writeFileSync(path.join(sessionDir, 'session.json'), JSON.stringify(session, null, 2), 'utf-8');
}

async function main() {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-session-lazy-'));
  const sessionsDir = path.join(tmpRoot, '.magi', 'sessions');
  fs.mkdirSync(sessionsDir, { recursive: true });

  const totalSessions = 60;
  const now = Date.now();
  for (let i = 0; i < totalSessions; i += 1) {
    writeSession(sessionsDir, `session-${i}`, now + i);
  }

  const { UnifiedSessionManager } = loadCompiledModule(path.join('session', 'unified-session-manager.js'));
  const manager = new UnifiedSessionManager(tmpRoot);

  const metas = manager.getSessionMetas();
  assert(metas.length === totalSessions, `元数据数量异常: ${metas.length}`);

  const max = manager.MAX_SESSIONS_IN_MEMORY;
  assert(typeof max === 'number' && max > 0, `MAX_SESSIONS_IN_MEMORY 异常: ${String(max)}`);
  assert(manager.sessions.size === max, `预载会话数量异常: ${manager.sessions.size}`);

  const oldestId = 'session-0';
  assert(!manager.sessions.has(oldestId), '最旧会话不应被预载');
  const loaded = manager.getSession(oldestId);
  assert(loaded && loaded.id === oldestId, '按需加载会话失败');
  assert(manager.sessions.has(oldestId), '按需加载后会话未进入缓存');
  assert(manager.sessions.size === max + 1, `按需加载缓存数量异常: ${manager.sessions.size}`);

  fs.rmSync(tmpRoot, { recursive: true, force: true });

  console.log('\n=== session 懒加载回归结果 ===');
  console.log(JSON.stringify({
    pass: true,
    totalSessions,
    maxInMemory: max,
    preloadedCount: max,
    loadedOnDemand: oldestId,
  }, null, 2));
  process.exit(0);
}

main().catch((error) => {
  console.error('session 懒加载回归失败:', error?.stack || error);
  process.exit(1);
});

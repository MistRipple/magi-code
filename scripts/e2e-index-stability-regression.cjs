#!/usr/bin/env node
/**
 * 本地索引稳定性回归
 *
 * 覆盖三组校验：
 * 1) 多语言增量更新（TS/PY/GO）
 * 2) 缓存命中与失效正确性
 * 3) session 切换隔离（复用现有回归脚本）
 */

const fs = require('fs');
const fsp = require('fs/promises');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function classifyType(filePath) {
  const normalized = filePath.replace(/\\/g, '/').toLowerCase();
  const base = path.basename(normalized);
  if (
    base.includes('.test.') ||
    base.includes('.spec.') ||
    normalized.includes('/test/') ||
    normalized.includes('/tests/') ||
    normalized.includes('/__tests__/')
  ) {
    return 'test';
  }
  const ext = path.extname(normalized);
  if (['.json', '.yaml', '.yml', '.toml', '.ini', '.env', '.cfg'].includes(ext)) {
    return 'config';
  }
  if ([
    '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs',
    '.py', '.go', '.java', '.rs',
    '.c', '.h', '.cpp', '.cc', '.cxx', '.hpp', '.hh',
    '.cs', '.php', '.rb', '.swift', '.kt', '.kts',
    '.m', '.mm', '.vue', '.svelte',
  ].includes(ext)) {
    return 'source';
  }
  return 'doc';
}

async function writeFile(absPath, content) {
  await fsp.mkdir(path.dirname(absPath), { recursive: true });
  await fsp.writeFile(absPath, content, 'utf-8');
}

function containsFile(results, relativePath) {
  return Array.isArray(results) && results.some((item) => item.filePath === relativePath);
}

async function runLocalIndexChecks() {
  const { LocalSearchEngine } = require(path.join(OUT, 'knowledge', 'local-search-engine.js'));
  const oldToken = 'staleprooftokenx111';
  const newToken = 'freshprooftokeny222';

  const tempRoot = await fsp.mkdtemp(path.join(os.tmpdir(), 'magi-index-stability-'));
  const relTs = 'src/alpha-handler.ts';
  const relPy = 'py/beta_worker.py';
  const relGo = 'go/gamma_worker.go';
  const absTs = path.join(tempRoot, relTs);
  const absPy = path.join(tempRoot, relPy);
  const absGo = path.join(tempRoot, relGo);

  try {
    await writeFile(absTs, `
export function alphaTokenHandler(): string {
  return 'alpha_token_v1';
}
`);

    await writeFile(absPy, `
def ${oldToken}():
    return "${oldToken}"
`);

    const engine = new LocalSearchEngine(tempRoot);
    await engine.buildIndex([
      { path: relTs, type: classifyType(relTs) },
      { path: relPy, type: classifyType(relPy) },
    ]);

    const initialTs = await engine.search('alphaTokenHandler');
    assert(containsFile(initialTs, relTs), 'TS 初始索引失败：未命中 alpha-handler.ts');

    const initialPy = await engine.search(oldToken);
    assert(containsFile(initialPy, relPy), 'PY 初始索引失败：未命中 beta_worker.py');

    // 缓存失效验证：先命中旧 token，再修改文件触发变更，再校验旧 token 不再命中。
    await engine.search(oldToken); // 预热缓存
    await writeFile(absPy, `
def ${newToken}():
    return "${newToken}"
`);
    engine.onFileChanged(absPy);

    const oldTokenAfterChange = await engine.search(oldToken);
    assert(!containsFile(oldTokenAfterChange, relPy), '缓存失效失败：旧 token 仍命中 beta_worker.py');

    const newTokenAfterChange = await engine.search(newToken);
    assert(containsFile(newTokenAfterChange, relPy), '增量更新失败：新 token 未命中 beta_worker.py');

    // 多语言 created 事件验证（GO）
    await writeFile(absGo, `
package main
func gamma_unique_entry() string { return "gamma_unique_entry" }
`);
    engine.onFileCreated(absGo);
    const goResults = await engine.search('gamma_unique_entry');
    assert(containsFile(goResults, relGo), '多语言 created 失败：未命中 gamma_worker.go');

    // deleted 事件验证
    await fsp.unlink(absPy);
    engine.onFileDeleted(absPy);
    const deletedResults = await engine.search(newToken);
    assert(!containsFile(deletedResults, relPy), 'deleted 事件失败：已删除文件仍被命中');

    return {
      pass: true,
      tempRoot,
      checks: {
        tsInitial: containsFile(initialTs, relTs),
        pyInitial: containsFile(initialPy, relPy),
        cacheInvalidation: !containsFile(oldTokenAfterChange, relPy),
        pyIncremental: containsFile(newTokenAfterChange, relPy),
        goCreated: containsFile(goResults, relGo),
        pyDeleted: !containsFile(deletedResults, relPy),
      },
    };
  } finally {
    await fsp.rm(tempRoot, { recursive: true, force: true });
  }
}

function runSessionIsolationCheck() {
  const scriptPath = path.join(ROOT, 'scripts', 'e2e-session-isolation-regression.cjs');
  const result = spawnSync(process.execPath, [scriptPath], {
    cwd: ROOT,
    encoding: 'utf-8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  return {
    pass: result.status === 0,
    exitCode: result.status,
    stdoutPreview: String(result.stdout || '').slice(-800),
    stderrPreview: String(result.stderr || '').slice(-800),
  };
}

async function main() {
  if (!fs.existsSync(path.join(OUT, 'knowledge', 'local-search-engine.js'))) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const localIndex = await runLocalIndexChecks();
  const sessionIsolation = runSessionIsolationCheck();

  const summary = {
    localIndex,
    sessionIsolation,
    pass: localIndex.pass && sessionIsolation.pass,
  };

  console.log('\n=== 本地索引稳定性回归结果 ===');
  console.log(JSON.stringify(summary, null, 2));

  process.exit(summary.pass ? 0 : 2);
}

main().catch((error) => {
  console.error('本地索引稳定性回归失败:', error?.stack || error);
  process.exitCode = 1;
});

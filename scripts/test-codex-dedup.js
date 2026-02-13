/**
 * Codex 工具去重机制验证测试
 *
 * 模拟 Codex 的典型行为模式，验证四层防御是否有效：
 * 1. 文件级去重 (viewedFiles)
 * 2. 分段级去重 (viewedRanges) — 精确去重 + ≥3段碎片化预警
 * 3. Jaccard 检索去重 (searchResultCache)
 * 4. 去重命中的递增惩罚 + 空转分数惩罚 (totalDedupHits/roundDedupHits)
 */

// ============================================================================
// 从 worker-adapter.ts 提取的核心逻辑（独立可运行，与源码保持一致）
// ============================================================================

class DedupSimulator {
  constructor() {
    this.searchResultCache = new Map();
    this.viewedFiles = new Set();
    this.viewedRanges = new Map();  // Map<string, Set<string>>
    this.totalDedupHits = 0;
    this.roundDedupHits = 0;
  }

  reset() {
    this.searchResultCache.clear();
    this.viewedFiles.clear();
    this.viewedRanges.clear();
    this.totalDedupHits = 0;
    this.roundDedupHits = 0;
  }

  resetRound() {
    this.roundDedupHits = 0;
  }

  isReadOnlyToolCall(toolCall) {
    const name = toolCall.name;
    const READ_ONLY_BUILTINS = [
      'codebase_retrieval', 'grep_search', 'list-processes',
      'read-process', 'web_search', 'web_fetch', 'mermaid_diagram',
    ];
    if (READ_ONLY_BUILTINS.includes(name)) return true;
    if (name === 'text_editor') {
      const command = toolCall.arguments?.command;
      return command === 'view' || command === 'list';
    }
    return false;
  }

  extractTargetFilePath(toolCall) {
    const args = toolCall.arguments || {};
    if (toolCall.name === 'text_editor' && (args.command === 'view' || args.command === 'list')) {
      return typeof args.path === 'string' ? args.path : null;
    }
    if (toolCall.name === 'grep_search' && typeof args.path === 'string') {
      if (/\.\w+$/.test(args.path)) return args.path;
    }
    return null;
  }

  extractQueryIdentifiers(toolCall) {
    const args = toolCall.arguments || {};
    const texts = [];
    for (const val of Object.values(args)) {
      if (typeof val === 'string') texts.push(val);
    }
    if (texts.length === 0) return [];
    const combined = texts.join(' ');
    const matches = combined.match(/[a-zA-Z_][\w.-]*/g) || [];
    const filtered = matches.filter(m => m.length >= 2);
    return [...new Set(filtered)].sort();
  }

  checkFileAccessDuplicate(toolCall) {
    if (!this.isReadOnlyToolCall(toolCall)) return null;
    const filePath = this.extractTargetFilePath(toolCall);
    if (!filePath) return null;

    // L1: 已完整读取 → 拦截所有后续访问
    if (this.viewedFiles.has(filePath)) {
      this.totalDedupHits++;
      this.roundDedupHits++;
      const basename = filePath.split('/').pop() || filePath;
      return `[系统提示] 文件 ${basename} 已被完整读取过`;
    }

    // L1+: 分段读取追踪（仅 text_editor view + view_range）
    if (toolCall.name === 'text_editor'
      && toolCall.arguments?.command === 'view'
      && toolCall.arguments?.view_range) {
      const range = toolCall.arguments.view_range;
      const rangeKey = Array.isArray(range) ? `${range[0]}-${range[1]}` : String(range);
      const fileRanges = this.viewedRanges.get(filePath);

      if (fileRanges) {
        // 精确去重：同一 range 已读取过
        if (fileRanges.has(rangeKey)) {
          this.totalDedupHits++;
          this.roundDedupHits++;
          const basename = filePath.split('/').pop() || filePath;
          return `[系统提示] 文件 ${basename} 的 ${rangeKey} 行已被读取过`;
        }
        // 碎片化预警：同一文件 ≥3 段
        if (fileRanges.size >= 3) {
          this.totalDedupHits++;
          this.roundDedupHits++;
          const basename = filePath.split('/').pop() || filePath;
          return `[系统提示] 你已对 ${basename} 进行了 ${fileRanges.size} 次分段读取`;
        }
      }
    }

    return null;
  }

  checkSearchDuplicate(toolCall) {
    if (!this.isReadOnlyToolCall(toolCall)) return null;

    // text_editor view：文件读取操作由 L1/L1+ 处理，跳过 Jaccard
    if (toolCall.name === 'text_editor'
      && toolCall.arguments?.command === 'view') {
      return null;
    }

    const newIdentifiers = this.extractQueryIdentifiers(toolCall);
    if (newIdentifiers.length === 0) return null;
    const newSet = new Set(newIdentifiers);

    for (const [cachedKey, cachedResult] of this.searchResultCache) {
      const cachedTokens = cachedKey.split('\x00');
      const cachedSet = new Set(cachedTokens);
      const intersection = [...newSet].filter(t => cachedSet.has(t)).length;
      const union = new Set([...newSet, ...cachedSet]).size;
      const similarity = intersection / union;

      if (similarity >= 0.4) {
        this.totalDedupHits++;
        this.roundDedupHits++;
        if (this.totalDedupHits >= 3) {
          return `[系统拒绝] 重复检索已被拦截（第 ${this.totalDedupHits} 次）`;
        }
        return `[系统提示] 已返回缓存结果`;
      }
    }
    return null;
  }

  recordFileAccess(toolCall) {
    const filePath = this.extractTargetFilePath(toolCall);
    if (!filePath) return;
    if (toolCall.name === 'text_editor' && toolCall.arguments?.command === 'view') {
      if (toolCall.arguments?.view_range) {
        // 分段读取 → 记录 range
        const range = toolCall.arguments.view_range;
        const rangeKey = Array.isArray(range) ? `${range[0]}-${range[1]}` : String(range);
        if (!this.viewedRanges.has(filePath)) {
          this.viewedRanges.set(filePath, new Set());
        }
        this.viewedRanges.get(filePath).add(rangeKey);
      } else {
        // 完整读取 → 记录文件
        this.viewedFiles.add(filePath);
      }
    }
  }

  cacheSearchResult(toolCall, result) {
    // text_editor view 由 L1/L1+ 处理，不污染搜索缓存
    if (toolCall.name === 'text_editor' && toolCall.arguments?.command === 'view') return;
    const identifiers = this.extractQueryIdentifiers(toolCall);
    if (identifiers.length === 0) return;
    const key = identifiers.join('\x00');
    this.searchResultCache.set(key, result);
  }

  /**
   * 模拟一次工具调用的完整流程
   * @returns { blocked: boolean, reason: string, message?: string }
   */
  simulateToolCall(toolCall, mockResult = 'mock content') {
    // 1. 文件级去重（含 L1+ 分段追踪）
    const fileDedup = this.checkFileAccessDuplicate(toolCall);
    if (fileDedup) {
      return { blocked: true, reason: 'file-dedup', message: fileDedup };
    }

    // 2. Jaccard 检索去重
    const searchDedup = this.checkSearchDuplicate(toolCall);
    if (searchDedup) {
      return { blocked: true, reason: 'jaccard-dedup', message: searchDedup };
    }

    // 3. 执行成功 → 缓存 + 记录
    if (this.isReadOnlyToolCall(toolCall)) {
      this.cacheSearchResult(toolCall, mockResult);
      this.recordFileAccess(toolCall);
    }

    return { blocked: false, reason: 'executed' };
  }
}

// ============================================================================
// 测试用例
// ============================================================================

let passed = 0;
let failed = 0;

function assert(condition, testName) {
  if (condition) {
    console.log(`  ✅ ${testName}`);
    passed++;
  } else {
    console.log(`  ❌ ${testName}`);
    failed++;
  }
}

const sim = new DedupSimulator();

// --------------------------------------------------------------------------
// 场景 1: 完整读取后重复 view 同一文件
// --------------------------------------------------------------------------
console.log('\n场景 1: 完整读取后重复 view 同一文件');
sim.reset();

const viewFile = { name: 'text_editor', id: '1', arguments: { command: 'view', path: '/src/types.ts' } };
let r1 = sim.simulateToolCall(viewFile);
assert(!r1.blocked, '首次 view /src/types.ts → 通过');

let r2 = sim.simulateToolCall(viewFile);
assert(r2.blocked && r2.reason === 'file-dedup', '第2次 view /src/types.ts → 文件级去重拦截');

// --------------------------------------------------------------------------
// 场景 2: 完整读取后用 grep 在同一文件搜索
// --------------------------------------------------------------------------
console.log('\n场景 2: 完整读取后 grep 搜索同一文件');
sim.reset();

const viewFirst = { name: 'text_editor', id: '2', arguments: { command: 'view', path: '/src/orchestrator/core/index.ts' } };
sim.simulateToolCall(viewFirst);

const grepSame = { name: 'grep_search', id: '3', arguments: { pattern: 'export', path: '/src/orchestrator/core/index.ts' } };
let r3 = sim.simulateToolCall(grepSame);
assert(r3.blocked && r3.reason === 'file-dedup', 'view 后 grep 同文件 → 文件级去重拦截');

// --------------------------------------------------------------------------
// 场景 3: 分段读取不同 range 不被拦截（≤2 段）
// --------------------------------------------------------------------------
console.log('\n场景 3: 分段读取不同 range 不被拦截');
sim.reset();

const viewRange1 = { name: 'text_editor', id: '4', arguments: { command: 'view', path: '/src/big-file.ts', view_range: [1, 100] } };
sim.simulateToolCall(viewRange1);

const viewRange2 = { name: 'text_editor', id: '5', arguments: { command: 'view', path: '/src/big-file.ts', view_range: [101, 200] } };
let r4 = sim.simulateToolCall(viewRange2);
assert(!r4.blocked, '分段读取第二段 → 不拦截');

// --------------------------------------------------------------------------
// 场景 4: 完整读取后分段读取被拦截
// --------------------------------------------------------------------------
console.log('\n场景 4: 完整读取后再分段读取被拦截');
sim.reset();

const viewFull = { name: 'text_editor', id: '6', arguments: { command: 'view', path: '/src/small-file.ts' } };
sim.simulateToolCall(viewFull);

const viewPartial = { name: 'text_editor', id: '7', arguments: { command: 'view', path: '/src/small-file.ts', view_range: [1, 50] } };
let r5 = sim.simulateToolCall(viewPartial);
assert(r5.blocked && r5.reason === 'file-dedup', '完整读取后分段读取 → 文件级去重拦截');

// --------------------------------------------------------------------------
// 场景 5: Jaccard 去重 — 换措辞搜索相同内容
// --------------------------------------------------------------------------
console.log('\n场景 5: 换措辞搜索相同内容');
sim.reset();

const search1 = { name: 'grep_search', id: '8', arguments: { pattern: 'WorkerAdapter sendMessage', path: '/src' } };
sim.simulateToolCall(search1, 'function sendMessage() { ... }');

const search2 = { name: 'grep_search', id: '9', arguments: { pattern: 'sendMessage WorkerAdapter', path: '/src' } };
let r6 = sim.simulateToolCall(search2);
assert(r6.blocked && r6.reason === 'jaccard-dedup', '换措辞搜索相同标识符 → Jaccard 去重拦截');

// --------------------------------------------------------------------------
// 场景 6: 不同内容的搜索不被拦截
// --------------------------------------------------------------------------
console.log('\n场景 6: 不同内容搜索不被拦截');
sim.reset();

const searchA = { name: 'grep_search', id: '10', arguments: { pattern: 'WorkerAdapter', path: '/src' } };
sim.simulateToolCall(searchA, 'class WorkerAdapter ...');

const searchB = { name: 'grep_search', id: '11', arguments: { pattern: 'ToolManager execute', path: '/src/tools' } };
let r7 = sim.simulateToolCall(searchB);
assert(!r7.blocked, '搜索完全不同的内容 → 不拦截');

// --------------------------------------------------------------------------
// 场景 7: 跨工具去重 — codebase_retrieval 搜索后 view 同一文件
// --------------------------------------------------------------------------
console.log('\n场景 7: codebase_retrieval 搜索后 view 同一文件');
sim.reset();

const codeSearch = { name: 'codebase_retrieval', id: '12', arguments: { query: 'handleMessage implementation' } };
sim.simulateToolCall(codeSearch, 'Found in message-hub.ts: ...');

const viewAfterSearch = { name: 'text_editor', id: '13', arguments: { command: 'view', path: '/src/message-hub.ts' } };
let r8 = sim.simulateToolCall(viewAfterSearch);
assert(!r8.blocked, 'codebase_retrieval 后 view → 通过（片段搜索不算完整读取）');

let r9 = sim.simulateToolCall(viewAfterSearch);
assert(r9.blocked && r9.reason === 'file-dedup', '完整 view 后再 view → 文件级去重拦截');

// --------------------------------------------------------------------------
// 场景 8: 去重递增惩罚 — 3次以上拒绝提供内容
// --------------------------------------------------------------------------
console.log('\n场景 8: 去重递增惩罚（3次后拒绝）');
sim.reset();

const baseSearch = { name: 'grep_search', id: '14', arguments: { pattern: 'checkStall detection', path: '/src' } };
sim.simulateToolCall(baseSearch, 'stall detection code...');

const dup1 = { name: 'grep_search', id: '15', arguments: { pattern: 'checkStall detection logic', path: '/src' } };
let d1 = sim.simulateToolCall(dup1);
assert(d1.blocked && d1.message.includes('缓存结果'), '第1次重复 → 返回缓存内容');

const dup2 = { name: 'grep_search', id: '16', arguments: { pattern: 'detection checkStall', path: '/src' } };
let d2 = sim.simulateToolCall(dup2);
assert(d2.blocked && d2.message.includes('缓存结果'), '第2次重复 → 返回缓存内容');

const dup3 = { name: 'grep_search', id: '17', arguments: { pattern: 'checkStall detection function', path: '/src' } };
let d3 = sim.simulateToolCall(dup3);
assert(d3.blocked && d3.message.includes('拒绝'), '第3次重复 → 拒绝（不再提供内容）');
assert(sim.totalDedupHits === 3, 'totalDedupHits === 3');

// --------------------------------------------------------------------------
// 场景 9: 写入操作不受去重影响
// --------------------------------------------------------------------------
console.log('\n场景 9: 写入操作不受去重影响');
sim.reset();

const viewForEdit = { name: 'text_editor', id: '18', arguments: { command: 'view', path: '/src/target.ts' } };
sim.simulateToolCall(viewForEdit);

const editFile = { name: 'text_editor', id: '19', arguments: { command: 'str_replace', path: '/src/target.ts', old_str: 'foo', new_str: 'bar' } };
let r10 = sim.simulateToolCall(editFile);
assert(!r10.blocked, '编辑操作（str_replace）→ 不受去重影响');

// --------------------------------------------------------------------------
// 场景 10: grep 搜索目录不被文件级去重拦截
// --------------------------------------------------------------------------
console.log('\n场景 10: grep 搜索目录 vs 搜索文件');
sim.reset();

const grepDir = { name: 'grep_search', id: '20', arguments: { pattern: 'export', path: '/src/orchestrator/' } };
sim.simulateToolCall(grepDir);
const grepDir2 = { name: 'grep_search', id: '21', arguments: { pattern: 'export class', path: '/src/orchestrator/' } };
let r11 = sim.simulateToolCall(grepDir2);
assert(!r11.blocked || r11.reason === 'jaccard-dedup', 'grep 目录 → 不走文件级去重（走 Jaccard）');

// --------------------------------------------------------------------------
// 场景 11: roundDedupHits 空转惩罚计算
// --------------------------------------------------------------------------
console.log('\n场景 11: roundDedupHits 空转惩罚计算');
sim.reset();

const file1 = { name: 'text_editor', id: '22', arguments: { command: 'view', path: '/src/a.ts' } };
sim.simulateToolCall(file1);

sim.resetRound();
sim.simulateToolCall(file1); // file-dedup hit → roundDedupHits=1
const file1grep = { name: 'grep_search', id: '23', arguments: { pattern: 'test', path: '/src/a.ts' } };
sim.simulateToolCall(file1grep); // file-dedup hit → roundDedupHits=2

const stallPenalty = sim.roundDedupHits * 2.0;
assert(sim.roundDedupHits === 2, `roundDedupHits === 2`);
assert(stallPenalty === 4.0, `空转额外惩罚 = ${stallPenalty}（预期 4.0）`);

// --------------------------------------------------------------------------
// 场景 12: 真实 Codex 行为模式模拟
// --------------------------------------------------------------------------
console.log('\n场景 12: 模拟 Codex 真实行为模式');
sim.reset();

const targetFile = '/src/llm/adapters/worker-adapter.ts';

const step1 = { name: 'text_editor', id: 'r1', arguments: { command: 'view', path: targetFile } };
let s1 = sim.simulateToolCall(step1, '// worker-adapter.ts full content...');
assert(!s1.blocked, 'R1: 首次完整读取 → 通过');

sim.resetRound();

const step2 = { name: 'grep_search', id: 'r2', arguments: { pattern: 'sendMessage', path: targetFile } };
let s2 = sim.simulateToolCall(step2);
assert(s2.blocked && s2.reason === 'file-dedup', 'R2: grep 已读文件 → 文件级拦截');

sim.resetRound();

const step3 = { name: 'grep_search', id: 'r3', arguments: { pattern: 'executeToolCalls', path: targetFile } };
let s3 = sim.simulateToolCall(step3);
assert(s3.blocked && s3.reason === 'file-dedup', 'R3: grep 已读文件（不同 pattern）→ 文件级拦截');

sim.resetRound();

const step4 = { name: 'text_editor', id: 'r4', arguments: { command: 'view', path: targetFile, view_range: [100, 200] } };
let s4 = sim.simulateToolCall(step4);
assert(s4.blocked && s4.reason === 'file-dedup', 'R4: view_range 已完整读取的文件 → 文件级拦截');

sim.resetRound();

const step5 = { name: 'text_editor', id: 'r5', arguments: { command: 'view', path: '/src/tools/tool-manager.ts' } };
let s5 = sim.simulateToolCall(step5);
assert(!s5.blocked, 'R5: 读取不同文件 → 通过');

assert(sim.totalDedupHits === 3, `总去重命中 = ${sim.totalDedupHits}（预期 3: R2+R3+R4）`);

// --------------------------------------------------------------------------
// 场景 13: 分段精确去重 — 同一 view_range 重复读取被拦截
// --------------------------------------------------------------------------
console.log('\n场景 13: 同一 view_range 精确去重');
sim.reset();

const rangeA1 = { name: 'text_editor', id: '30', arguments: { command: 'view', path: '/src/big.ts', view_range: [1, 100] } };
let ra1 = sim.simulateToolCall(rangeA1);
assert(!ra1.blocked, '首次 view_range [1,100] → 通过');

let ra2 = sim.simulateToolCall(rangeA1);
assert(ra2.blocked && ra2.reason === 'file-dedup', '第二次 view_range [1,100] → 精确去重拦截');
assert(ra2.message.includes('1-100'), '拦截消息包含 range 信息');

// --------------------------------------------------------------------------
// 场景 14: 碎片化读取预警 — 同一文件 ≥3 段分段读取
// --------------------------------------------------------------------------
console.log('\n场景 14: 碎片化读取预警（≥3 段）');
sim.reset();

const frag1 = { name: 'text_editor', id: '31', arguments: { command: 'view', path: '/src/huge.ts', view_range: [1, 50] } };
const frag2 = { name: 'text_editor', id: '32', arguments: { command: 'view', path: '/src/huge.ts', view_range: [51, 100] } };
const frag3 = { name: 'text_editor', id: '33', arguments: { command: 'view', path: '/src/huge.ts', view_range: [101, 150] } };
const frag4 = { name: 'text_editor', id: '34', arguments: { command: 'view', path: '/src/huge.ts', view_range: [151, 200] } };

let f1 = sim.simulateToolCall(frag1);
assert(!f1.blocked, '第1段 → 通过');
let f2 = sim.simulateToolCall(frag2);
assert(!f2.blocked, '第2段 → 通过');
let f3 = sim.simulateToolCall(frag3);
assert(!f3.blocked, '第3段 → 通过');
// 第4段：已有3段记录，触发碎片化预警
let f4 = sim.simulateToolCall(frag4);
assert(f4.blocked && f4.reason === 'file-dedup', '第4段 → 碎片化预警拦截');
assert(f4.message.includes('3 次分段读取'), '拦截消息提示碎片化次数');

// --------------------------------------------------------------------------
// 场景 15: 分段读取后完整读取不被拦截
// --------------------------------------------------------------------------
console.log('\n场景 15: 分段读取后完整读取不被拦截');
sim.reset();

const partial1 = { name: 'text_editor', id: '35', arguments: { command: 'view', path: '/src/mid.ts', view_range: [1, 50] } };
sim.simulateToolCall(partial1);

const fullRead = { name: 'text_editor', id: '36', arguments: { command: 'view', path: '/src/mid.ts' } };
let fr = sim.simulateToolCall(fullRead);
assert(!fr.blocked, '分段读取后完整读取 → 通过');

// 完整读取后再分段读取应被 L1 拦截
const partial2 = { name: 'text_editor', id: '37', arguments: { command: 'view', path: '/src/mid.ts', view_range: [51, 100] } };
let fr2 = sim.simulateToolCall(partial2);
assert(fr2.blocked && fr2.reason === 'file-dedup', '完整读取后再分段读取 → 文件级拦截');

// --------------------------------------------------------------------------
// 场景 16: 不同文件的分段读取互不影响
// --------------------------------------------------------------------------
console.log('\n场景 16: 不同文件分段读取互不影响');
sim.reset();

const fileA = { name: 'text_editor', id: '38', arguments: { command: 'view', path: '/src/fileA.ts', view_range: [1, 50] } };
const fileB = { name: 'text_editor', id: '39', arguments: { command: 'view', path: '/src/fileB.ts', view_range: [1, 50] } };
sim.simulateToolCall(fileA);
let fb = sim.simulateToolCall(fileB);
assert(!fb.blocked, '不同文件的相同 range → 不拦截');

// ============================================================================
// 结果汇总
// ============================================================================
console.log('\n' + '='.repeat(60));
console.log(`结果: ${passed} 通过, ${failed} 失败 (共 ${passed + failed} 项)`);
console.log('='.repeat(60));

if (failed > 0) {
  process.exit(1);
}

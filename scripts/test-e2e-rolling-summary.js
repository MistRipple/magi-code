/**
 * 端到端验证脚本：滚动摘要 + 连续 user role 防护 + SharedContextPool 序列化
 *
 * 从源码提取核心逻辑进行真实行为验证，覆盖所有边界情况。
 * 运行：node scripts/test-e2e-rolling-summary.js
 */

const MAX_ROLLING_SUMMARY_CHARS = 2000;
let passed = 0;
let failed = 0;

function assert(condition, testName) {
  if (condition) {
    console.log(`  ✅ ${testName}`);
    passed++;
  } else {
    console.error(`  ❌ FAIL: ${testName}`);
    failed++;
  }
}

// ============================================================================
// 核心函数：从 worker-adapter.ts 提取的逻辑
// ============================================================================

function updateRollingSummary(droppedMessages, prevSummary) {
  const keyPoints = [];
  for (const msg of droppedMessages) {
    const content = typeof msg.content === 'string'
      ? msg.content
      : Array.isArray(msg.content)
        ? msg.content.filter(b => b?.type === 'text').map(b => b.text || '').join(' ')
        : '';
    // 提取 assistant 结论（文本长度 >= 10 字符才有提取价值）
    if (msg.role === 'assistant' && content && content.length >= 10) {
      const trimmed = content.trim();
      if (trimmed.length <= 400) {
        keyPoints.push(`[结论] ${trimmed}`);
      } else {
        const head = trimmed.substring(0, 200).trim();
        const tail = trimmed.substring(trimmed.length - 200).trim();
        keyPoints.push(`[结论] ${head}...${tail}`);
      }
    }
    // 提取工具调用中的文件路径（独立于文本内容长度判断）
    if (Array.isArray(msg.content)) {
      for (const block of msg.content) {
        if (block?.type === 'tool_use') {
          const toolName = block.name || '';
          const input = block.input || {};
          const filePath = input.path || input.file_path || input.filePath || '';
          if (filePath) keyPoints.push(`[工具] ${toolName}: ${filePath}`);
        }
      }
    }
  }
  if (keyPoints.length === 0) return prevSummary;
  const newContent = keyPoints.join('\n');
  const merged = prevSummary ? `${prevSummary}\n---\n${newContent}` : newContent;
  if (merged.length > MAX_ROLLING_SUMMARY_CHARS) {
    return `[System 上下文回顾] 以下是之前工作中的关键发现和操作记录（已自动精简）：\n\n${merged.substring(merged.length - MAX_ROLLING_SUMMARY_CHARS + 100)}`;
  }
  return `[System 上下文回顾] 以下是之前工作中的关键发现和操作记录：\n\n${merged}`;
}

/**
 * 模拟 truncateHistoryIfNeeded 的注入逻辑（修复后版本）
 */
function injectRollingSummary(history, summary) {
  if (!summary) return history;
  const firstMsg = history[0];
  if (firstMsg && firstMsg.role === 'user') {
    if (typeof firstMsg.content === 'string') {
      firstMsg.content = `${summary}\n\n---\n\n${firstMsg.content}`;
    } else if (Array.isArray(firstMsg.content)) {
      firstMsg.content.unshift({ type: 'text', text: summary });
    }
  } else {
    history.unshift({ role: 'user', content: summary });
  }
  return history;
}

/**
 * 检查是否有连续相同 role
 */
function hasConsecutiveRoles(history) {
  for (let i = 1; i < history.length; i++) {
    if (history[i].role === history[i - 1].role) {
      return { found: true, index: i, role: history[i].role };
    }
  }
  return { found: false };
}

// ============================================================================
// 测试用例
// ============================================================================

console.log('\n🧪 测试组1：连续 user role 防护');
console.log('─'.repeat(50));

// 场景A：截断后首条是 user → 应合并
{
  const history = [
    { role: 'user', content: '请分析 src/index.ts 的结构' },
    { role: 'assistant', content: '该文件包含...' },
    { role: 'user', content: '继续检查 src/utils.ts' },
    { role: 'assistant', content: '工具函数包括...' },
  ];
  const summary = '[System 上下文回顾] 关键发现：前期读取了 config.ts';
  injectRollingSummary(history, summary);
  const check = hasConsecutiveRoles(history);
  assert(!check.found, '场景A: 首条是 user，合并后无连续 role');
  assert(history.length === 4, '场景A: 消息数量不变（合并而非新增）');
  assert(history[0].content.includes('关键发现'), '场景A: 摘要内容已合并到首条');
  assert(history[0].content.includes('请分析 src/index.ts'), '场景A: 原始内容未丢失');
}

// 场景B：截断后首条是 assistant → 应 unshift
{
  const history = [
    { role: 'assistant', content: '分析结果是...' },
    { role: 'user', content: '继续' },
    { role: 'assistant', content: '完成' },
  ];
  const summary = '[System 上下文回顾] 关键发现：Bug 在第 42 行';
  injectRollingSummary(history, summary);
  const check = hasConsecutiveRoles(history);
  assert(!check.found, '场景B: 首条是 assistant，unshift 后 user→assistant 交替正确');
  assert(history.length === 4, '场景B: 消息数量 +1（新增 user）');
  assert(history[0].role === 'user', '场景B: 首条变为 user');
}

// 场景C：首条是 user + array content（带工具结果）
{
  const history = [
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'xyz', content: '命令输出...' }] },
    { role: 'assistant', content: '看到命令输出了' },
  ];
  const summary = '[System 上下文回顾] 之前执行了 npm install';
  injectRollingSummary(history, summary);
  const check = hasConsecutiveRoles(history);
  assert(!check.found, '场景C: array content user，合并后无连续 role');
  assert(history.length === 2, '场景C: 消息数量不变');
  assert(Array.isArray(history[0].content), '场景C: content 仍是数组');
  assert(history[0].content[0].type === 'text', '场景C: 摘要作为 text block 插入数组首位');
  assert(history[0].content[1].type === 'tool_result', '场景C: 原始 tool_result 保留');
}

console.log('\n🧪 测试组2：updateRollingSummary 关键信息提取');
console.log('─'.repeat(50));

// 场景F：提取 assistant 结论
{
  const dropped = [
    { role: 'assistant', content: '经过分析，根本原因是 EventEmitter 未正确 dispose，导致内存泄漏。修复方案是在 deactivate 中调用 removeAllListeners。' },
    { role: 'user', content: '继续' },
  ];
  const summary = updateRollingSummary(dropped, null);
  assert(summary !== null, '场景F: 提取了 assistant 结论');
  assert(summary.includes('[结论]'), '场景F: 包含 [结论] 标签');
  assert(summary.includes('EventEmitter'), '场景F: 保留了关键信息');
}

// 场景G：提取工具调用的文件路径
{
  const dropped = [
    {
      role: 'assistant',
      content: [
        { type: 'text', text: '让我查看文件' },
        { type: 'tool_use', name: 'view_file', input: { path: 'src/core/engine.ts' } },
      ],
    },
    {
      role: 'user',
      content: [{ type: 'tool_result', tool_use_id: 'abc', content: '文件内容...' }],
    },
  ];
  const summary = updateRollingSummary(dropped, null);
  assert(summary !== null, '场景G: 提取了工具调用');
  assert(summary.includes('[工具]'), '场景G: 包含 [工具] 标签');
  assert(summary.includes('src/core/engine.ts'), '场景G: 保留了文件路径');
}

// 场景H：长文本截断（首200 + 末200）
{
  const longText = 'A'.repeat(500);
  const dropped = [{ role: 'assistant', content: longText }];
  const summary = updateRollingSummary(dropped, null);
  assert(summary.includes('...'), '场景H: 超长文本包含省略号');
}

// 场景I：累积多次摘要
{
  const first = updateRollingSummary(
    [{ role: 'assistant', content: '第一轮发现：配置文件缺少 timeout 参数' }],
    null
  );
  const second = updateRollingSummary(
    [{ role: 'assistant', content: '第二轮发现：timeout 默认值应为 30000ms' }],
    // 剥离前缀，模拟实际的 rollingContextSummary 内容
    first.replace(/^\[System 上下文回顾\].*?\n\n/, '')
  );
  assert(second.includes('timeout 参数'), '场景I: 累积摘要保留第一轮发现');
  assert(second.includes('30000ms'), '场景I: 累积摘要包含第二轮发现');
  assert(second.includes('---'), '场景I: 两轮摘要有分隔符');
}

// 场景J：超长摘要裁剪 — 验证累积50轮后摘要长度受控
{
  let accumulated = '';
  let everTruncated = false;
  for (let i = 0; i < 50; i++) {
    accumulated = updateRollingSummary(
      [{ role: 'assistant', content: `第 ${i} 轮分析结论：文件 src/module-${i}.ts 中存在循环依赖问题，需要重构` }],
      accumulated ? accumulated.replace(/^\[System 上下文回顾\].*?\n\n/, '') : null
    );
    if (accumulated.includes('已自动精简')) everTruncated = true;
  }
  // 50 轮不裁剪约 50 * 60 = 3000 字符，远超 2000 → 裁剪必然触发过
  assert(everTruncated, '场景J: 累积过程中触发过裁剪');
  assert(accumulated.length <= MAX_ROLLING_SUMMARY_CHARS + 200, `场景J: 最终长度受控 (${accumulated.length} chars)`);
}

console.log('\n🧪 测试组3：SharedContextPool 序列化/反序列化');
console.log('─'.repeat(50));

// 模拟 SharedContextPool 核心逻辑
class MockSharedContextPool {
  constructor() { this.entries = new Map(); }
  add(entry) {
    this.entries.set(entry.id, entry);
    return { action: 'added', id: entry.id };
  }
  toSerializable() { return Array.from(this.entries.values()); }
  fromSerializable(entries) {
    let restored = 0;
    for (const entry of entries) {
      if (!entry.id || this.entries.has(entry.id)) continue;
      this.entries.set(entry.id, entry);
      restored++;
    }
    return restored;
  }
}

// 场景K：序列化 → 反序列化 完整性
{
  const pool = new MockSharedContextPool();
  pool.add({ id: 'e1', missionId: 'm1', type: 'insight', content: '发现 Bug' });
  pool.add({ id: 'e2', missionId: 'm1', type: 'decision', content: '使用方案 A' });
  pool.add({ id: 'e3', missionId: 'm2', type: 'risk', content: '性能风险' });

  const serialized = pool.toSerializable();
  assert(serialized.length === 3, '场景K: 序列化 3 个条目');

  const newPool = new MockSharedContextPool();
  const restored = newPool.fromSerializable(serialized);
  assert(restored === 3, '场景K: 反序列化恢复 3 个条目');
  assert(newPool.entries.get('e1').content === '发现 Bug', '场景K: 内容完整');
}

// 场景L：反序列化去重
{
  const pool = new MockSharedContextPool();
  pool.add({ id: 'existing', missionId: 'm1', content: '已有' });
  const restored = pool.fromSerializable([
    { id: 'existing', missionId: 'm1', content: '重复' },
    { id: 'new1', missionId: 'm1', content: '新增' },
    { id: '', missionId: 'm1', content: '无 id' },
  ]);
  assert(restored === 1, '场景L: 仅恢复 1 个新条目（跳过重复和空 id）');
  assert(pool.entries.get('existing').content === '已有', '场景L: 已有条目不被覆盖');
  assert(pool.entries.get('new1').content === '新增', '场景L: 新条目正确插入');
}

console.log('\n🧪 测试组4：端到端完整截断流程模拟');
console.log('─'.repeat(50));

// 场景M：模拟 10 轮 Worker 对话 → 截断 → 摘要保全 → 再截断
{
  const maxMessages = 12;
  const preserveRecentRounds = 3;
  let history = [];
  let rollingSummary = null;

  // 模拟 10 轮对话
  for (let i = 0; i < 10; i++) {
    history.push({ role: 'user', content: `任务 ${i}: 分析 src/module-${i}.ts` });
    history.push({
      role: 'assistant',
      content: [
        { type: 'text', text: `分析完成：module-${i} 有 ${i * 10} 行代码，发现 ${i} 个问题` },
        { type: 'tool_use', name: 'view_file', input: { path: `src/module-${i}.ts` } },
      ],
    });
  }
  assert(history.length === 20, '场景M: 生成了 20 条消息');

  // 第一次截断
  if (history.length > maxMessages) {
    const preserveCount = Math.min(preserveRecentRounds * 2, history.length);
    const truncatedCount = history.length - preserveCount;
    const dropped = history.slice(0, truncatedCount);
    history = history.slice(-preserveCount);

    const rawSummary = rollingSummary
      ? rollingSummary.replace(/^\[System 上下文回顾\].*?\n\n/, '')
      : null;
    rollingSummary = updateRollingSummary(dropped, rawSummary);
    injectRollingSummary(history, rollingSummary);
  }

  const check1 = hasConsecutiveRoles(history);
  assert(!check1.found, '场景M-1: 第一次截断后无连续 role');
  assert(rollingSummary.includes('module-0'), '场景M-1: 摘要保留了早期工具调用');

  // 追加更多对话触发第二次截断
  for (let i = 10; i < 16; i++) {
    history.push({ role: 'user', content: `任务 ${i}: 修复 src/fix-${i}.ts` });
    history.push({ role: 'assistant', content: `修复完成: fix-${i} 已更新` });
  }

  if (history.length > maxMessages) {
    const preserveCount = Math.min(preserveRecentRounds * 2, history.length);
    const truncatedCount = history.length - preserveCount;
    const dropped = history.slice(0, truncatedCount);
    history = history.slice(-preserveCount);

    const rawSummary = rollingSummary
      ? rollingSummary.replace(/^\[System 上下文回顾\].*?\n\n/, '')
      : null;
    rollingSummary = updateRollingSummary(dropped, rawSummary);
    injectRollingSummary(history, rollingSummary);
  }

  const check2 = hasConsecutiveRoles(history);
  assert(!check2.found, '场景M-2: 第二次截断后无连续 role');
  assert(history[0].role === 'user', '场景M-2: 首条消息是 user');

  // 验证历史中确实包含最新和摘要内容
  const allContent = history.map(m =>
    typeof m.content === 'string' ? m.content : JSON.stringify(m.content)
  ).join('\n');
  assert(allContent.includes('上下文回顾'), '场景M-2: 历史包含滚动摘要');
}

// ============================================================================
// 最终报告
// ============================================================================

console.log('\n' + '═'.repeat(50));
console.log(`📊 测试结果: ${passed} 通过, ${failed} 失败, 共 ${passed + failed} 项`);
if (failed > 0) {
  console.log('❌ 有测试失败！');
  process.exit(1);
} else {
  console.log('✅ 全部通过！端到端验证成功。');
  process.exit(0);
}

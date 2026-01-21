/**
 * 测试代码块解析是否正确保留首行
 * 直接实现解析逻辑进行测试
 */

console.log('='.repeat(60));
console.log('代码块首行解析测试');
console.log('='.repeat(60));

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    console.log(`✅ ${name}`);
    passed++;
  } catch (e) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${e.message}`);
    failed++;
  }
}

// 直接复制 extractCodeBlocks 的正则逻辑
function extractCodeBlocks(content) {
  const blocks = [];
  // 使用与 content-parser.ts 相同的正则
  const regex = /```(\w*)(?::([^\s\n]+)|[^\S\n]+([^\n]*))?\n([\s\S]*?)```/g;
  let match;

  while ((match = regex.exec(content)) !== null) {
    blocks.push({
      lang: match[1] || 'text',
      code: match[4] || '',
      startIndex: match.index,
      endIndex: match.index + match[0].length,
    });
  }

  return blocks;
}

// 测试 1: 简单 JSON 代码块
test('简单 JSON 代码块保留首行 {', () => {
  const content = '```json\n{\n  "goal": "test"\n}\n```';
  const blocks = extractCodeBlocks(content);

  if (blocks.length !== 1) throw new Error(`应该有 1 个代码块，实际 ${blocks.length}`);
  if (blocks[0].lang !== 'json') throw new Error(`语言应该是 json，实际 ${blocks[0].lang}`);

  const code = blocks[0].code;
  console.log('   代码内容:', JSON.stringify(code));

  if (!code.includes('{')) throw new Error('代码应该包含 {');
  if (!code.trim().startsWith('{')) throw new Error(`代码应该以 { 开头，实际以 "${code.trim()[0]}" 开头`);
});

// 测试 2: 复杂 JSON 规划输出
test('复杂 JSON 规划输出保留首行 {', () => {
  const content = `\`\`\`json
{
  "goal": "创建一个单人坦克大战小游戏",
  "analysis": "中等复杂度游戏开发任务",
  "constraints": [
    "技术栈: Python FastAPI + Vue 3"
  ]
}
\`\`\``;

  const blocks = extractCodeBlocks(content);
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');

  const firstLine = blocks[0].code.split('\n')[0];
  console.log('   首行:', JSON.stringify(firstLine));

  if (firstLine.trim() !== '{') throw new Error(`首行应该是 {，实际是 "${firstLine}"`);
});

// 测试 3: 混合内容中的 JSON 代码块
test('混合内容中的 JSON 代码块', () => {
  const content = `我来分析一下这个任务：

\`\`\`json
{
  "goal": "创建一个单人坦克大战小游戏",
  "constraints": ["c1", "c2"]
}
\`\`\`

任务分析完成。`;

  const blocks = extractCodeBlocks(content);
  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');

  const code = blocks[0].code;
  const firstChar = code.trim()[0];
  console.log('   代码首字符:', JSON.stringify(firstChar));
  console.log('   代码前 50 字符:', JSON.stringify(code.substring(0, 50)));

  if (firstChar !== '{') throw new Error(`代码应该以 { 开头，实际以 "${firstChar}" 开头`);
});

// 测试 4: 用户截图中的实际内容（模拟）
test('用户截图中的 25 行 JSON', () => {
  const jsonContent = `{
  "goal": "创建一个单人坦克大战小游戏，包含Python后端（FastAPI）和Vue前端，玩家控制坦克消灭AI敌人",
  "analysis": "中等复杂度游戏开发任务。后端已完成（FastAPI配置API、排行榜）。前端缺少核心游戏引擎: Canvas渲染、输入处理、游戏循环",
  "constraints": [
    "技术栈: Python FastAPI + Vue 3",
    "单人游戏模式",
    "简单版本",
    "后端已完成，需完成前端游戏引擎"
  ],
  "acceptanceCriteria": [
    "后端服务可启动 ✅ 已完成",
    "前端应用可运行",
    "玩家坦克可移动（WASD）和射击（空格）",
    "敌人坦克有AI行为",
    "碰撞检测正常",
    "胜负判定正确",
    "游戏可重新开始"
  ],
  "riskLevel": "medium",
  "riskFactors": [
    "前端游戏引擎尚未实现",
    "Canvas渲染和游戏循环需要较多代码",
    "AI逻辑和碰撞检测需要调试"
  ]
}`;

  const content = '```json\n' + jsonContent + '\n```';
  const blocks = extractCodeBlocks(content);

  if (blocks.length !== 1) throw new Error('应该有 1 个代码块');

  const lines = blocks[0].code.split('\n');
  console.log('   行数:', lines.length);
  console.log('   第1行:', JSON.stringify(lines[0]));
  console.log('   第2行:', JSON.stringify(lines[1]));

  if (lines[0].trim() !== '{') throw new Error(`第一行应该是 {，实际是 "${lines[0]}"`);
});

// 测试 5: 检查截图中显示的问题 - 没有 { 的情况
test('检测问题：如果原始内容没有 {', () => {
  // 模拟可能的问题场景：JSON 内容不包含开头的 {
  const problematicContent = `\`\`\`json
  "goal": "创建一个单人坦克大战小游戏",
  "analysis": "中等复杂度"
\`\`\``;

  const blocks = extractCodeBlocks(problematicContent);
  const firstLine = blocks[0].code.split('\n')[0];
  console.log('   问题场景首行:', JSON.stringify(firstLine));

  // 这种情况下首行不是 {，说明问题在原始内容
  if (firstLine.trim().startsWith('"goal"')) {
    console.log('   ⚠️ 原始内容就没有 {，这可能是问题根源');
  }
});

console.log('\n' + '='.repeat(60));
console.log(`测试结果: ${passed} 通过, ${failed} 失败`);
console.log('='.repeat(60));

// 额外诊断：检查前端可能的问题
console.log('\n诊断信息:');
console.log('如果后端解析正确但前端显示不完整，可能的原因：');
console.log('1. 前端 renderCodeBlock 的 trimmedCode 处理有问题');
console.log('2. highlight.js 高亮后首行被误处理');
console.log('3. CSS 样式导致首行不可见');
console.log('4. 可折叠面板的预览逻辑有问题');

process.exit(failed > 0 ? 1 : 0);

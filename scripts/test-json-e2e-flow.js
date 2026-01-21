/**
 * 端到端测试：模拟完整的 JSON 规划输出数据流
 * 从原始 CLI 输出到前端显示
 */

console.log('='.repeat(60));
console.log('JSON 规划输出端到端测试');
console.log('='.repeat(60));

// 直接复制关键函数
function extractEmbeddedJson(content) {
  const results = [];
  let i = 0;
  while (i < content.length) {
    const char = content[i];
    // 跳过代码块中的内容
    if (content.substring(i, i + 3) === '```') {
      const endIndex = content.indexOf('```', i + 3);
      if (endIndex !== -1) {
        i = endIndex + 3;
        continue;
      }
    }
    if (char === '{' || char === '[') {
      const extracted = tryExtractJsonAt(content, i);
      if (extracted) {
        results.push(extracted);
        i = extracted.endIndex;
        continue;
      }
    }
    i++;
  }
  return results;
}

function tryExtractJsonAt(content, startIndex) {
  const startChar = content[startIndex];
  const endChar = startChar === '{' ? '}' : ']';
  let depth = 0;
  let inString = false;
  let escapeNext = false;
  for (let i = startIndex; i < content.length; i++) {
    const char = content[i];
    if (escapeNext) { escapeNext = false; continue; }
    if (char === '\\') { escapeNext = true; continue; }
    if (char === '"') { inString = !inString; continue; }
    if (inString) continue;
    if (char === startChar) depth++;
    else if (char === endChar) {
      depth--;
      if (depth === 0) {
        const jsonText = content.substring(startIndex, i + 1);
        try {
          JSON.parse(jsonText);
          return { jsonText, startIndex, endIndex: i + 1 };
        } catch { return null; }
      }
    }
  }
  return null;
}

function extractCodeBlocks(content) {
  const blocks = [];
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

// 测试场景
console.log('\n场景 1: 带代码块的 JSON 规划输出');
console.log('-'.repeat(40));
{
  const rawContent = `我来分析一下这个任务。

\`\`\`json
{
  "goal": "创建一个单人坦克大战小游戏",
  "analysis": "中等复杂度游戏开发任务"
}
\`\`\`

分析完成。`;

  console.log('原始内容长度:', rawContent.length);

  // Step 1: extractEmbeddedJson（应该跳过代码块内的 JSON）
  const embeddedJsons = extractEmbeddedJson(rawContent);
  console.log('裸露 JSON 数量:', embeddedJsons.length);

  // Step 2: 移除裸露 JSON 后的内容
  let content = rawContent;
  for (let i = embeddedJsons.length - 1; i >= 0; i--) {
    const json = embeddedJsons[i];
    const before = content.substring(0, json.startIndex).trimEnd();
    const after = content.substring(json.endIndex).trimStart();
    content = before + (before && after ? '\n\n' : '') + after;
  }
  console.log('移除裸露 JSON 后内容长度:', content.length);

  // Step 3: 提取代码块
  const codeBlocks = extractCodeBlocks(content);
  console.log('代码块数量:', codeBlocks.length);

  if (codeBlocks.length > 0) {
    console.log('代码块语言:', codeBlocks[0].lang);
    console.log('代码块内容首行:', JSON.stringify(codeBlocks[0].code.split('\n')[0]));
    console.log('代码块首字符:', JSON.stringify(codeBlocks[0].code.trim()[0]));

    if (codeBlocks[0].code.trim().startsWith('{')) {
      console.log('✅ 代码块正确以 { 开头');
    } else {
      console.log('❌ 代码块没有以 { 开头!');
    }
  }
}

console.log('\n场景 2: 纯 JSON（无代码块包裹）');
console.log('-'.repeat(40));
{
  const rawContent = `{
  "goal": "创建一个单人坦克大战小游戏",
  "analysis": "中等复杂度游戏开发任务",
  "constraints": [
    "技术栈: Python FastAPI + Vue 3"
  ]
}`;

  console.log('原始内容长度:', rawContent.length);
  console.log('原始内容首字符:', JSON.stringify(rawContent.trim()[0]));

  // Step 1: extractEmbeddedJson
  const embeddedJsons = extractEmbeddedJson(rawContent);
  console.log('裸露 JSON 数量:', embeddedJsons.length);

  if (embeddedJsons.length > 0) {
    console.log('裸露 JSON 首字符:', JSON.stringify(embeddedJsons[0].jsonText.trim()[0]));
    console.log('⚠️ 纯 JSON 会被移除！这可能是问题所在');
  }

  // Step 2: 移除裸露 JSON 后的内容
  let content = rawContent;
  for (let i = embeddedJsons.length - 1; i >= 0; i--) {
    const json = embeddedJsons[i];
    const before = content.substring(0, json.startIndex).trimEnd();
    const after = content.substring(json.endIndex).trimStart();
    content = before + (before && after ? '\n\n' : '') + after;
  }
  console.log('移除裸露 JSON 后内容长度:', content.length);
  console.log('移除后内容:', JSON.stringify(content));
}

console.log('\n场景 3: 混合内容 - 文本 + 裸露 JSON');
console.log('-'.repeat(40));
{
  const rawContent = `我来分析一下这个任务。

{
  "goal": "创建一个单人坦克大战小游戏",
  "analysis": "中等复杂度游戏开发任务"
}

分析完成。`;

  console.log('原始内容长度:', rawContent.length);

  // Step 1: extractEmbeddedJson
  const embeddedJsons = extractEmbeddedJson(rawContent);
  console.log('裸露 JSON 数量:', embeddedJsons.length);

  if (embeddedJsons.length > 0) {
    console.log('裸露 JSON 首字符:', JSON.stringify(embeddedJsons[0].jsonText.trim()[0]));
    console.log('⚠️ 裸露 JSON 会被移除！');
  }

  // Step 2: 移除裸露 JSON 后的内容
  let content = rawContent;
  for (let i = embeddedJsons.length - 1; i >= 0; i--) {
    const json = embeddedJsons[i];
    const before = content.substring(0, json.startIndex).trimEnd();
    const after = content.substring(json.endIndex).trimStart();
    content = before + (before && after ? '\n\n' : '') + after;
  }
  console.log('移除裸露 JSON 后的内容:');
  console.log(content);
}

console.log('\n场景 4: 用户截图场景 - 代码块但首行缺失');
console.log('-'.repeat(40));
{
  // 假设 CLI 输出了这样的内容（首行 { 在代码块标记后立即出现）
  const rawContent = `\`\`\`json
{
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
}
\`\`\``;

  console.log('原始内容长度:', rawContent.length);

  // Step 1: extractEmbeddedJson（应该跳过代码块）
  const embeddedJsons = extractEmbeddedJson(rawContent);
  console.log('裸露 JSON 数量:', embeddedJsons.length);

  // Step 2: 提取代码块
  const codeBlocks = extractCodeBlocks(rawContent);
  console.log('代码块数量:', codeBlocks.length);

  if (codeBlocks.length > 0) {
    const code = codeBlocks[0].code;
    const lines = code.split('\n');
    console.log('代码块行数:', lines.length);
    console.log('代码块首行:', JSON.stringify(lines[0]));
    console.log('代码块第二行:', JSON.stringify(lines[1]));

    if (lines[0].trim() === '{') {
      console.log('✅ 代码块首行正确是 {');
    } else {
      console.log('❌ 代码块首行不是 {!');
      console.log('   首行实际内容:', JSON.stringify(lines[0]));
    }
  }
}

console.log('\n场景 5: 检查正则匹配边界');
console.log('-'.repeat(40));
{
  // 测试各种边界情况
  const testCases = [
    '```json\n{\n  "a": 1\n}\n```',
    '```json\n\n{\n  "a": 1\n}\n```',  // 代码块开头有空行
    '```json\r\n{\r\n  "a": 1\r\n}\r\n```',  // Windows 换行
  ];

  for (let i = 0; i < testCases.length; i++) {
    const content = testCases[i];
    console.log(`\n测试用例 ${i + 1}:`);
    console.log('原始内容:', JSON.stringify(content));

    const blocks = extractCodeBlocks(content);
    if (blocks.length > 0) {
      const firstLine = blocks[0].code.split('\n')[0];
      console.log('代码块首行:', JSON.stringify(firstLine));
      console.log('首行 trim 后:', JSON.stringify(firstLine.trim()));

      if (firstLine.trim() === '{' || firstLine.trim() === '') {
        console.log('✅ 首行符合预期');
      } else {
        console.log('❌ 首行不符合预期');
      }
    } else {
      console.log('❌ 未找到代码块');
    }
  }
}

console.log('\n' + '='.repeat(60));
console.log('测试完成');
console.log('='.repeat(60));

console.log('\n结论：');
console.log('如果所有场景中代码块首行都正确包含 {，');
console.log('那么问题可能在于：');
console.log('1. CLI 原始输出就没有正确包含 {');
console.log('2. 前端接收到的数据已经被截断');
console.log('3. 某个序列化/反序列化步骤丢失了数据');

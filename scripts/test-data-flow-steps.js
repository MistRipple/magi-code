/**
 * 数据流分步验证测试
 *
 * 步骤：
 * 1. 模拟 CLI 原始输出 - 验证是否完整
 * 2. 经过后端 parseContentToBlocks 处理 - 验证处理结果
 * 3. 模拟前端渲染逻辑 - 验证渲染结果
 */

const fs = require('fs');
const path = require('path');

console.log('='.repeat(70));
console.log('数据流分步验证测试');
console.log('='.repeat(70));

// ========================================
// Step 1: 模拟各种 CLI 原始输出格式
// ========================================
console.log('\n【步骤 1】CLI 原始数据格式验证');
console.log('-'.repeat(50));

const cliOutputs = [
  {
    name: 'JSON 代码块（标准格式）',
    content: `\`\`\`json
{
  "goal": "创建一个单人坦克大战小游戏",
  "analysis": "中等复杂度游戏开发任务",
  "constraints": [
    "技术栈: Python FastAPI + Vue 3",
    "单人游戏模式"
  ]
}
\`\`\``,
    expectedFirstLine: '{'
  },
  {
    name: 'Python 代码块',
    content: `\`\`\`python
def hello():
    print("Hello World")
\`\`\``,
    expectedFirstLine: 'def hello():'
  },
  {
    name: 'Markdown 内容',
    content: `## 任务分析

这是一个**重要**的任务。

1. 第一步
2. 第二步
`,
    expectedFirstLine: '## 任务分析'
  },
  {
    name: '混合内容（文本 + 代码块）',
    content: `我来分析这个任务：

\`\`\`json
{
  "goal": "测试目标"
}
\`\`\`

分析完成。`,
    expectedFirstLine: '{'  // 代码块的首行
  },
  {
    name: '25行 JSON（模拟截图场景）',
    content: `\`\`\`json
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
\`\`\``,
    expectedFirstLine: '{'
  }
];

// 验证 CLI 原始数据
for (const cli of cliOutputs) {
  console.log(`\n▶ ${cli.name}:`);
  console.log(`  原始内容长度: ${cli.content.length} 字符`);

  // 检查代码块内容
  const codeBlockMatch = cli.content.match(/```(\w*)\n([\s\S]*?)```/);
  if (codeBlockMatch) {
    const lang = codeBlockMatch[1];
    const code = codeBlockMatch[2];
    const firstLine = code.split('\n')[0];

    console.log(`  代码块语言: ${lang}`);
    console.log(`  代码块首行: ${JSON.stringify(firstLine)}`);
    console.log(`  期望首行: ${JSON.stringify(cli.expectedFirstLine)}`);

    if (firstLine.trim() === cli.expectedFirstLine || firstLine === cli.expectedFirstLine) {
      console.log(`  ✅ CLI 原始数据正确`);
    } else {
      console.log(`  ❌ CLI 原始数据不正确！`);
    }
  } else {
    console.log(`  无代码块，跳过首行检查`);
    console.log(`  ✅ CLI 原始数据完整`);
  }
}

// ========================================
// Step 2: 后端处理验证
// ========================================
console.log('\n\n【步骤 2】后端处理结果验证');
console.log('-'.repeat(50));

// 简化版的 extractCodeBlocks（与 content-parser.ts 相同）
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

// 简化版的 extractEmbeddedJson
function extractEmbeddedJson(content) {
  const results = [];
  let i = 0;
  while (i < content.length) {
    if (content.substring(i, i + 3) === '```') {
      const endIdx = content.indexOf('```', i + 3);
      if (endIdx !== -1) { i = endIdx + 3; continue; }
    }
    if (content[i] === '{' || content[i] === '[') {
      const extracted = tryExtractJson(content, i);
      if (extracted) { results.push(extracted); i = extracted.endIndex; continue; }
    }
    i++;
  }
  return results;
}

function tryExtractJson(content, start) {
  const startChar = content[start];
  const endChar = startChar === '{' ? '}' : ']';
  let depth = 0, inStr = false, esc = false;
  for (let i = start; i < content.length; i++) {
    if (esc) { esc = false; continue; }
    if (content[i] === '\\') { esc = true; continue; }
    if (content[i] === '"') { inStr = !inStr; continue; }
    if (inStr) continue;
    if (content[i] === startChar) depth++;
    else if (content[i] === endChar) {
      depth--;
      if (depth === 0) {
        const text = content.substring(start, i + 1);
        try { JSON.parse(text); return { jsonText: text, startIndex: start, endIndex: i + 1 }; }
        catch { return null; }
      }
    }
  }
  return null;
}

// 简化版的 parseContentToBlocks
function parseContentToBlocks(rawContent) {
  if (!rawContent) return [];

  let content = rawContent;
  const blocks = [];

  // 移除裸露 JSON
  const embeddedJsons = extractEmbeddedJson(content);
  for (let i = embeddedJsons.length - 1; i >= 0; i--) {
    const json = embeddedJsons[i];
    const before = content.substring(0, json.startIndex).trimEnd();
    const after = content.substring(json.endIndex).trimStart();
    content = before + (before && after ? '\n\n' : '') + after;
  }

  // 提取代码块
  const codeBlocks = extractCodeBlocks(content);

  if (codeBlocks.length > 0) {
    let lastIndex = 0;
    for (const cb of codeBlocks) {
      if (cb.startIndex > lastIndex) {
        const text = content.slice(lastIndex, cb.startIndex).trim();
        if (text) blocks.push({ type: 'text', content: text, isMarkdown: true });
      }
      blocks.push({ type: 'code', content: cb.code, language: cb.lang });
      lastIndex = cb.endIndex;
    }
    if (lastIndex < content.length) {
      const text = content.slice(lastIndex).trim();
      if (text) blocks.push({ type: 'text', content: text, isMarkdown: true });
    }
  } else {
    const trimmed = content.trim();
    if (trimmed) {
      blocks.push({ type: 'text', content: trimmed, isMarkdown: true });
    }
  }

  return blocks;
}

// 验证后端处理结果
for (const cli of cliOutputs) {
  console.log(`\n▶ ${cli.name}:`);

  const blocks = parseContentToBlocks(cli.content);
  console.log(`  解析后块数量: ${blocks.length}`);

  for (let i = 0; i < blocks.length; i++) {
    const block = blocks[i];
    console.log(`  块 ${i + 1}: type=${block.type}, language=${block.language || 'N/A'}`);

    if (block.type === 'code') {
      const lines = block.content.split('\n');
      const firstLine = lines[0];
      console.log(`    代码行数: ${lines.length}`);
      console.log(`    首行内容: ${JSON.stringify(firstLine)}`);

      if (firstLine.trim() === cli.expectedFirstLine || firstLine === cli.expectedFirstLine) {
        console.log(`    ✅ 后端处理正确，首行完整`);
      } else {
        console.log(`    ❌ 后端处理错误！首行丢失或不正确`);
        console.log(`    期望: ${JSON.stringify(cli.expectedFirstLine)}`);
      }
    }
  }
}

// ========================================
// Step 3: 前端渲染验证
// ========================================
console.log('\n\n【步骤 3】前端渲染结果验证');
console.log('-'.repeat(50));

// 简化版的 renderCodeBlock 逻辑
function simulateRenderCodeBlock(code, lang) {
  const trimmedCode = code.replace(/^\n+/, '').replace(/\n+$/, '');
  const lines = trimmedCode.split('\n');

  return {
    lineCount: lines.length,
    firstLine: lines[0],
    lastLine: lines[lines.length - 1],
    previewLines: lines.slice(0, 5)
  };
}

for (const cli of cliOutputs) {
  const blocks = parseContentToBlocks(cli.content);
  const codeBlock = blocks.find(b => b.type === 'code');

  if (codeBlock) {
    console.log(`\n▶ ${cli.name}:`);

    const rendered = simulateRenderCodeBlock(codeBlock.content, codeBlock.language);
    console.log(`  渲染行数: ${rendered.lineCount}`);
    console.log(`  渲染首行: ${JSON.stringify(rendered.firstLine)}`);

    if (rendered.firstLine.trim() === cli.expectedFirstLine || rendered.firstLine === cli.expectedFirstLine) {
      console.log(`  ✅ 前端渲染正确`);
    } else {
      console.log(`  ❌ 前端渲染错误！首行丢失`);
    }

    if (rendered.lineCount > 20) {
      console.log(`  注意: 超过20行，会使用可折叠面板`);
      console.log(`  预览行 (前5行):`);
      rendered.previewLines.forEach((line, i) => {
        console.log(`    ${i + 1}: ${JSON.stringify(line)}`);
      });
    }
  }
}

console.log('\n' + '='.repeat(70));
console.log('验证完成');
console.log('='.repeat(70));

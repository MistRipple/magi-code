#!/usr/bin/env node

const fs = require('fs');

console.log('提取辅助函数...\n');

// 读取 index.html
const content = fs.readFileSync('src/ui/webview/index.html', 'utf-8');
const lines = content.split('\n');

// 找到主 <script> 标签
let scriptStart = -1;
let scriptEnd = -1;
let scriptCount = 0;

for (let i = 0; i < lines.length; i++) {
  if (lines[i].trim() === '<script>') {
    scriptCount++;
    if (scriptCount === 2) {
      scriptStart = i;
    }
  }
  if (lines[i].trim() === '</script>' && scriptStart !== -1 && scriptEnd === -1) {
    scriptEnd = i;
    break;
  }
}

const jsLines = lines.slice(scriptStart + 1, scriptEnd);
const jsContent = jsLines.join('\n');

// 需要提取的辅助函数
const helperFunctions = [
  'collectWorkerStatusEntries',
  'getMessageGroupKey',
  'getRoleIcon',
  'getRoleInfo',
  'getToolIcon',
  'formatTime',
  'cleanInternalProtocolData',
  'normalizeMessageContentForDedup',
  'findEquivalentMessage',
  'isInternalJsonMessage',
  'isJsonText',
  'formatSimpleContent',
  'getModeDisplayName',
  'getLatestTaskStats',
  'formatTokenCount',
  'updateRelativeTimes',
  'updateStreamingHints',
  'scheduleRenderMainContent'
];

// 提取函数代码
function extractFunction(funcName) {
  const regex = new RegExp(`function\\s+${funcName}\\s*\\([^)]*\\)\\s*\\{`, 'g');
  const match = regex.exec(jsContent);

  if (!match) {
    console.log(`⚠️  未找到函数: ${funcName}`);
    return null;
  }

  const startPos = match.index;
  let braceCount = 0;
  let inFunction = false;
  let endPos = startPos;

  for (let i = startPos; i < jsContent.length; i++) {
    const char = jsContent[i];
    if (char === '{') {
      braceCount++;
      inFunction = true;
    } else if (char === '}') {
      braceCount--;
      if (inFunction && braceCount === 0) {
        endPos = i + 1;
        break;
      }
    }
  }

  const funcCode = jsContent.substring(startPos, endPos);
  const lineCount = (funcCode.match(/\n/g) || []).length + 1;

  return {
    name: funcName,
    code: funcCode,
    lines: lineCount
  };
}

const extractedFunctions = [];
let totalLines = 0;

helperFunctions.forEach(funcName => {
  const func = extractFunction(funcName);
  if (func) {
    extractedFunctions.push(func);
    totalLines += func.lines;
    console.log(`✅ ${funcName}() - ${func.lines} 行`);
  }
});

console.log(`\n总计: ${extractedFunctions.length} 个辅助函数, ${totalLines} 行\n`);

// 输出到文件供检查
let output = '// 提取的辅助函数\n\n';
extractedFunctions.forEach(func => {
  output += func.code + '\n\n';
});

fs.writeFileSync('/tmp/helper-functions.js', output, 'utf-8');
console.log('✅ 辅助函数已保存到 /tmp/helper-functions.js');

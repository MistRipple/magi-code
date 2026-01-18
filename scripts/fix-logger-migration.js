#!/usr/bin/env node

/**
 * 修复日志迁移中的问题
 */

const fs = require('fs');
const path = require('path');

function fixFile(filePath) {
  let content = fs.readFileSync(filePath, 'utf8');
  let modified = false;

  // 1. 修复导入路径错误
  // 'logging' -> './logging' 或 '../logging' 等
  const depth = filePath.split('/').length - 2;
  const correctPath = '../'.repeat(depth) + 'logging';

  if (content.includes("from 'logging'")) {
    content = content.replace(/from 'logging'/g, `from '${correctPath}'`);
    modified = true;
  }
  if (content.includes('from "logging"')) {
    content = content.replace(/from "logging"/g, `from "${correctPath}"`);
    modified = true;
  }

  // 2. 修复 logger.error 的参数问题
  // logger.error('msg', undefined, LogCategory.X) -> logger.error('msg', undefined, LogCategory.X)
  // logger.error('msg', error, undefined, LogCategory.X) -> logger.error('msg', error, LogCategory.X)

  // 匹配 logger.error 调用，移除多余的 undefined
  content = content.replace(
    /logger\.error\(([^,]+),\s*([^,]+),\s*undefined,\s*(LogCategory\.\w+)\)/g,
    'logger.error($1, $2, $3)'
  );

  // 3. 修复 logger.info/warn/debug 的参数问题
  // 移除模板字符串中错误插入的参数
  content = content.replace(
    /logger\.(info|warn|debug)\(`([^`]*)\$\{([^}]+)\(,\s*undefined,\s*LogCategory\.\w+\)([^`]*)`\)/g,
    (match, method, before, funcCall, after) => {
      return `logger.${method}(\`${before}\${${funcCall}()}${after}\`, undefined, LogCategory.ORCHESTRATOR)`;
    }
  );

  if (modified || content !== fs.readFileSync(filePath, 'utf8')) {
    fs.writeFileSync(filePath, content, 'utf8');
    return true;
  }
  return false;
}

function main() {
  console.log('=== 修复日志迁移问题 ===\n');

  // 查找所有 TypeScript 文件
  const files = [];
  function findFiles(dir) {
    const entries = fs.readdirSync(dir, { withFileTypes: true });
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name !== 'node_modules' && entry.name !== 'test') {
          findFiles(fullPath);
        }
      } else if (entry.isFile() && entry.name.endsWith('.ts') && !entry.name.endsWith('.bak.ts')) {
        files.push(fullPath);
      }
    }
  }

  findFiles('src');

  console.log(`找到 ${files.length} 个文件\n`);

  let fixedCount = 0;
  for (const file of files) {
    if (fixFile(file)) {
      console.log(`✓ 修复: ${file}`);
      fixedCount++;
    }
  }

  console.log(`\n修复完成: ${fixedCount} 个文件`);
}

main();

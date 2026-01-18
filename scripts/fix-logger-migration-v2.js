#!/usr/bin/env node

/**
 * 修复日志迁移中的所有问题
 */

const fs = require('fs');
const path = require('path');

function fixFile(filePath) {
  let content = fs.readFileSync(filePath, 'utf8');
  const originalContent = content;

  // 1. 修复导入路径错误 'logging' -> './logging' 或 '../logging'
  const depth = filePath.split('/').length - 2;
  const correctPath = '../'.repeat(depth) + 'logging';

  content = content.replace(/from ['"]logging['"]/g, `from '${correctPath}'`);

  // 2. 修复 logger 调用的参数问题
  // 移除所有 logger 调用中多余的 undefined 参数

  // logger.info('msg', data, undefined, LogCategory.X) -> logger.info('msg', data, LogCategory.X)
  content = content.replace(
    /logger\.(info|warn|debug)\(([^)]+),\s*undefined,\s*(LogCategory\.\w+)\)/g,
    'logger.$1($2, $3)'
  );

  // logger.error('msg', error, undefined, LogCategory.X) -> logger.error('msg', error, LogCategory.X)
  content = content.replace(
    /logger\.error\(([^)]+),\s*undefined,\s*(LogCategory\.\w+)\)/g,
    'logger.error($1, $2)'
  );

  // 3. 修复有 5 个参数的情况（错误的迁移）
  // logger.info('msg', data, undefined, undefined, LogCategory.X) -> logger.info('msg', data, LogCategory.X)
  content = content.replace(
    /logger\.(info|warn|debug)\(([^,]+),\s*([^,]+),\s*undefined,\s*undefined,\s*(LogCategory\.\w+)\)/g,
    'logger.$1($2, $3, $4)'
  );

  // 4. 修复有 4 个参数但第二个是 undefined 的情况
  // logger.info('msg', undefined, undefined, LogCategory.X) -> logger.info('msg', undefined, LogCategory.X)
  content = content.replace(
    /logger\.(info|warn|debug)\(([^,]+),\s*undefined,\s*undefined,\s*(LogCategory\.\w+)\)/g,
    'logger.$1($2, undefined, $3)'
  );

  // 5. 修复 logger.error 有 4 个参数的情况
  // logger.error('msg', error, undefined, LogCategory.X) -> logger.error('msg', error, LogCategory.X)
  content = content.replace(
    /logger\.error\(([^,]+),\s*([^,]+),\s*undefined,\s*(LogCategory\.\w+)\)/g,
    'logger.error($1, $2, $3)'
  );

  // 6. 修复只有 2 个参数但应该有 3 个的情况（缺少 undefined）
  // logger.info('msg', LogCategory.X) -> logger.info('msg', undefined, LogCategory.X)
  content = content.replace(
    /logger\.(info|warn|debug)\(([^,)]+),\s*(LogCategory\.\w+)\)/g,
    (match, method, msg, category) => {
      // 检查 msg 是否已经包含逗号（说明有 data 参数）
      if (msg.includes(',')) {
        return match; // 已经有 data 参数，不修改
      }
      return `logger.${method}(${msg}, undefined, ${category})`;
    }
  );

  // 7. 修复 logger.error 只有 2 个参数的情况
  // logger.error('msg', LogCategory.X) -> logger.error('msg', undefined, LogCategory.X)
  content = content.replace(
    /logger\.error\(([^,)]+),\s*(LogCategory\.\w+)\)/g,
    (match, msg, category) => {
      if (msg.includes(',')) {
        return match;
      }
      return `logger.error(${msg}, undefined, ${category})`;
    }
  );

  if (content !== originalContent) {
    fs.writeFileSync(filePath, content, 'utf8');
    return true;
  }
  return false;
}

function main() {
  console.log('=== 修复日志迁移问题（完整版）===\n');

  const files = [];
  function findFiles(dir) {
    const entries = fs.readdirSync(dir, { withFileTypes: true });
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name !== 'node_modules' && entry.name !== 'test' && entry.name !== 'logging') {
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

#!/usr/bin/env node

/**
 * 智能日志迁移工具
 * 自动将 console.* 替换为 logger.*，并根据文件路径和上下文推断分类
 */

const fs = require('fs');
const path = require('path');

// 文件路径到日志分类的映射
const PATH_TO_CATEGORY = {
  'cli/': 'LogCategory.CLI',
  'task/': 'LogCategory.TASK',
  'worker': 'LogCategory.WORKER',
  'orchestrator': 'LogCategory.ORCHESTRATOR',
  'session': 'LogCategory.SESSION',
  'recovery': 'LogCategory.RECOVERY',
  'ui/': 'LogCategory.UI',
};

// 关键词到日志分类的映射
const KEYWORD_TO_CATEGORY = {
  'CLI': 'LogCategory.CLI',
  'Task': 'LogCategory.TASK',
  'Worker': 'LogCategory.WORKER',
  'Orchestrator': 'LogCategory.ORCHESTRATOR',
  'Session': 'LogCategory.SESSION',
  'Recovery': 'LogCategory.RECOVERY',
  'WebviewProvider': 'LogCategory.UI',
};

function inferCategory(filePath, line) {
  // 1. 根据文件路径推断
  for (const [pathPattern, category] of Object.entries(PATH_TO_CATEGORY)) {
    if (filePath.includes(pathPattern)) {
      return category;
    }
  }

  // 2. 根据日志内容推断
  for (const [keyword, category] of Object.entries(KEYWORD_TO_CATEGORY)) {
    if (line.includes(keyword)) {
      return category;
    }
  }

  // 3. 默认为 SYSTEM
  return 'LogCategory.SYSTEM';
}

function migrateFile(filePath) {
  console.log(`  处理: ${filePath}`);

  let content = fs.readFileSync(filePath, 'utf8');
  const originalContent = content;
  let modified = false;

  // 检查是否已经导入 logger
  const hasLoggerImport = /from ['"]\.\.?\/.*logging['"]/.test(content);

  if (!hasLoggerImport) {
    // 计算相对路径
    const depth = filePath.split('/').length - 2; // src/ 算一层
    const relativePath = '../'.repeat(depth) + 'logging';

    // 找到第一个 import 语句
    const importMatch = content.match(/^import .+;$/m);
    if (importMatch) {
      const importIndex = content.indexOf(importMatch[0]);
      content =
        content.slice(0, importIndex) +
        `import { logger, LogCategory } from '${relativePath}';\n` +
        content.slice(importIndex);
      modified = true;
    } else {
      // 如果没有 import，在文件开头添加
      content = `import { logger, LogCategory } from '${relativePath}';\n\n` + content;
      modified = true;
    }
  }

  // 替换 console 调用
  const lines = content.split('\n');
  const newLines = lines.map((line, index) => {
    let newLine = line;

    // console.debug -> logger.debug
    if (line.includes('console.debug(')) {
      const category = inferCategory(filePath, line);
      newLine = line.replace(/console\.debug\(/g, `logger.debug(`);
      // 如果不是默认分类，添加分类参数
      if (category !== 'LogCategory.SYSTEM') {
        newLine = newLine.replace(/logger\.debug\(([^)]+)\)/, `logger.debug($1, undefined, ${category})`);
      }
      modified = true;
    }

    // console.log -> logger.info
    if (line.includes('console.log(')) {
      const category = inferCategory(filePath, line);
      newLine = line.replace(/console\.log\(/g, `logger.info(`);
      if (category !== 'LogCategory.SYSTEM') {
        newLine = newLine.replace(/logger\.info\(([^)]+)\)/, `logger.info($1, undefined, ${category})`);
      }
      modified = true;
    }

    // console.warn -> logger.warn
    if (line.includes('console.warn(')) {
      const category = inferCategory(filePath, line);
      newLine = line.replace(/console\.warn\(/g, `logger.warn(`);
      if (category !== 'LogCategory.SYSTEM') {
        newLine = newLine.replace(/logger\.warn\(([^)]+)\)/, `logger.warn($1, undefined, ${category})`);
      }
      modified = true;
    }

    // console.error -> logger.error
    if (line.includes('console.error(')) {
      const category = inferCategory(filePath, line);
      newLine = line.replace(/console\.error\(/g, `logger.error(`);
      if (category !== 'LogCategory.SYSTEM') {
        newLine = newLine.replace(/logger\.error\(([^)]+)\)/, `logger.error($1, undefined, ${category})`);
      }
      modified = true;
    }

    return newLine;
  });

  if (modified) {
    content = newLines.join('\n');
    fs.writeFileSync(filePath, content, 'utf8');
    console.log(`    ✓ 已迁移`);
    return true;
  } else {
    console.log(`    - 无需修改`);
    return false;
  }
}

function findFilesToMigrate(dir, files = []) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });

  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);

    if (entry.isDirectory()) {
      // 跳过特定目录
      if (entry.name === 'node_modules' || entry.name === 'logging' || entry.name === 'test') {
        continue;
      }
      findFilesToMigrate(fullPath, files);
    } else if (entry.isFile() && entry.name.endsWith('.ts') && !entry.name.endsWith('.bak.ts')) {
      // 检查文件是否包含 console 调用
      const content = fs.readFileSync(fullPath, 'utf8');
      if (/console\.(log|warn|error|debug)\(/.test(content)) {
        files.push(fullPath);
      }
    }
  }

  return files;
}

function main() {
  console.log('=== 智能日志迁移工具 ===\n');

  // 查找需要迁移的文件
  console.log('1. 扫描文件...');
  const files = findFilesToMigrate('src');
  console.log(`  找到 ${files.length} 个文件需要迁移\n`);

  if (files.length === 0) {
    console.log('没有需要迁移的文件');
    return;
  }

  // 显示文件列表
  console.log('2. 文件列表:');
  files.forEach(file => console.log(`  - ${file}`));
  console.log('');

  // 迁移文件
  console.log('3. 开始迁移...');
  let migratedCount = 0;
  for (const file of files) {
    if (migrateFile(file)) {
      migratedCount++;
    }
  }

  console.log('');
  console.log('=== ✅ 迁移完成 ===');
  console.log('');
  console.log(`迁移摘要:`);
  console.log(`  - 扫描文件: ${files.length}`);
  console.log(`  - 已迁移: ${migratedCount}`);
  console.log(`  - 未修改: ${files.length - migratedCount}`);
  console.log('');
  console.log('注意事项:');
  console.log('  1. 请手动检查迁移后的代码');
  console.log('  2. 运行 npx tsc --noEmit 检查编译');
  console.log('  3. 运行测试确保功能正常');
  console.log('');
}

main();

#!/usr/bin/env node

/**
 * 检查未使用的导入（简单检查）
 */

const fs = require('fs');

console.log('🔍 检查未使用的导入...\n');

const jsFiles = [
  'src/ui/webview/js/ui/message-renderer.js',
  'src/ui/webview/js/ui/message-handler.js',
  'src/ui/webview/js/ui/event-handlers.js',
  'src/ui/webview/js/main.js'
];

let hasUnused = false;

jsFiles.forEach(file => {
  const content = fs.readFileSync(file, 'utf-8');
  
  // 提取导入的名称
  const imports = [];
  const importRegex = /import\s+\{([^}]+)\}\s+from/g;
  let match;
  
  while ((match = importRegex.exec(content)) !== null) {
    const names = match[1].split(',').map(n => n.trim());
    imports.push(...names);
  }
  
  // 检查每个导入是否在代码中使用
  const unused = [];
  imports.forEach(name => {
    // 简单检查：在导入语句之外是否出现该名称
    const importLine = content.indexOf(`import`);
    const afterImports = content.substring(importLine + 500); // 跳过导入区域
    
    // 检查是否作为函数调用、变量使用等
    const usagePattern = new RegExp(`\\b${name}\\b`, 'g');
    const matches = afterImports.match(usagePattern);
    
    if (!matches || matches.length === 0) {
      unused.push(name);
    }
  });
  
  if (unused.length > 0) {
    console.log(`⚠️  ${file}:`);
    console.log(`   可能未使用: ${unused.join(', ')}`);
    console.log(`   (注意：这是简单检查，可能有误报)`);
    hasUnused = true;
  } else {
    console.log(`✅ ${file} (${imports.length} 个导入都在使用)`);
  }
});

console.log('\n' + '='.repeat(60));

if (hasUnused) {
  console.log('⚠️  发现可能未使用的导入（可能有误报）');
  console.log('   建议手动检查确认');
} else {
  console.log('✅ 所有导入都在使用');
}

console.log('='.repeat(60));

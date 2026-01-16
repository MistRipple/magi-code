/**
 * UI 渲染功能测试脚本
 * 模拟 CLI 输出各种格式的内容，验证前端渲染效果
 */

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

// 模拟的测试消息
const testMessages = [
  // 1. 代码块测试 - 带行号和文件路径
  {
    type: 'code_block',
    content: `这是一个代码示例：

\`\`\`typescript:src/utils/helper.ts
import { Logger } from './logger';

export function formatDate(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return \`\${year}-\${month}-\${day}\`;
}

export function debounce<T extends (...args: any[]) => any>(
  fn: T,
  delay: number
): (...args: Parameters<T>) => void {
  let timer: NodeJS.Timeout | null = null;
  return (...args: Parameters<T>) => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => fn(...args), delay);
  };
}
\`\`\`
`
  },

  // 2. 差异高亮测试
  {
    type: 'diff_block',
    content: `修改了配置文件：

\`\`\`diff:config/settings.json
 {
   "name": "MultiCLI",
-  "version": "0.1.0",
+  "version": "0.2.0",
   "features": {
-    "codeHighlight": false,
+    "codeHighlight": true,
+    "lineNumbers": true,
+    "diffColors": true,
     "darkMode": true
   }
 }
\`\`\`
`
  },

  // 3. 长消息测试（超过15行应该折叠）
  {
    type: 'long_message',
    content: `这是一个很长的消息，用于测试自动折叠功能：

第1行内容
第2行内容
第3行内容
第4行内容
第5行内容
第6行内容
第7行内容
第8行内容
第9行内容
第10行内容
第11行内容
第12行内容
第13行内容
第14行内容
第15行内容
第16行内容 - 超过15行了
第17行内容
第18行内容
第19行内容
第20行内容

这条消息应该被自动折叠，显示"展开更多"按钮。`
  },

  // 4. 任务进度测试
  {
    type: 'task_progress',
    content: JSON.stringify({
      type: 'task_card',
      cli: 'claude',
      status: 'started',
      description: '实现用户登录功能',
      progress: 2,
      total: 4,
      subtasks: [
        { cli: 'claude', description: '设计数据库模型', status: 'completed' },
        { cli: 'codex', description: '实现后端API', status: 'completed' },
        { cli: 'gemini', description: '创建前端组件', status: 'running' },
        { cli: 'claude', description: '编写测试用例', status: 'pending' }
      ]
    })
  },

  // 5. 相对时间测试
  {
    type: 'relative_time',
    content: '这条消息用于测试相对时间显示',
    timestamp: Date.now() - 120000 // 2分钟前
  }
];

// 输出测试消息
console.log('============================================================');
console.log('  UI 渲染功能测试');
console.log('============================================================\n');

testMessages.forEach((msg, idx) => {
  console.log(`\n--- 测试 ${idx + 1}: ${msg.type} ---\n`);
  console.log(msg.content);
  if (msg.timestamp) {
    console.log(`[timestamp: ${msg.timestamp}]`);
  }
  console.log('\n');
});

console.log('============================================================');
console.log('  测试完成 - 请在 VS Code 扩展中查看渲染效果');
console.log('============================================================');

